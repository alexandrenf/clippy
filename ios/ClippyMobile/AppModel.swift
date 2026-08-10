@preconcurrency import Combine
import Foundation
import Network
import UniformTypeIdentifiers
import UIKit

@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var syncState: SyncState = .idle
    @Published private(set) var library: LocalSyncView = .empty
    @Published private(set) var connectingAccount = false
    @Published var message: String?

    let auth: AuthController
    private let configuration: RuntimeConfiguration
    private let keychain = KeychainStore()
    private let cloud: ConvexSyncClient
    private var deviceActorId: String
    private var workspaceId: String?
    private var store: LocalSyncStore?
    private var cloudAuthenticated = false
    private var cloudCounters: [String: UInt64] = [:]
    private var cloudAuthenticationTask: Task<Void, Never>?
    private var cloudAuthenticationRetryTask: Task<Void, Never>?
    private var transientSyncRetryTask: Task<Void, Never>?
    private var accountEnrollmentTask: Task<Void, Never>?
    private var safetyPollTask: Task<Void, Never>?
    private var exchangeTask: Task<Void, Never>?
    private var authSubscription: AnyCancellable?
    private var changesSubscription: AnyCancellable?
    private let pathMonitor = NWPathMonitor()
    private let pathQueue = DispatchQueue(label: "app.clippy.mobile.convex-path")
    private var isForeground = true
    private var isOnline = true
    private var exchangeInProgress = false
    private var syncRequested = false
    private var retryIndex = 0
    private var authenticationRetryIndex = 0
    private var exchangeGeneration = UUID()
    private var confirmedRemoteChunks: Set<String> = []

    init(configuration: RuntimeConfiguration) {
        self.configuration = configuration
        let auth = AuthController(configuration: configuration)
        self.auth = auth
        cloud = ConvexSyncClient(deploymentURL: configuration.convexURL) {
            try await auth.accessToken()
        }
        if let data = try? keychain.load(account: configuration.keychainAccount("device-actor")),
           let value = String(data: data, encoding: .utf8), UUID(uuidString: value) != nil {
            deviceActorId = value.lowercased()
        } else {
            let value = UUID().uuidString.lowercased()
            deviceActorId = value
            try? keychain.save(Data(value.utf8), account: configuration.keychainAccount("device-actor"))
        }
        if let data = try? keychain.load(account: configuration.keychainAccount("workspace-id")),
           let workspace = String(data: data, encoding: .utf8),
           !workspace.isEmpty {
            workspaceId = workspace
            syncState = .waitingForDevice
            let acceptancePending = (try? keychain.load(
                account: configuration.keychainAccount("pending-enrollment-id")
            )) != nil
            if !acceptancePending {
                Task { [weak self] in await self?.openReplica(workspaceId: workspace) }
            }
        }
        authSubscription = auth.$signedIn
            .removeDuplicates()
            .sink { [weak self] signedIn in
                Task { @MainActor in
                    if signedIn {
                        self?.ensureCloudAuthentication()
                    } else {
                        self?.cloudAuthenticated = false
                        self?.stopForegroundNetworking()
                    }
                }
            }
        pathMonitor.pathUpdateHandler = { [weak self] path in
            Task { @MainActor in self?.networkPathChanged(path.status == .satisfied) }
        }
        pathMonitor.start(queue: pathQueue)
    }

    deinit {
        safetyPollTask?.cancel()
        exchangeTask?.cancel()
        transientSyncRetryTask?.cancel()
        cloudAuthenticationTask?.cancel()
        cloudAuthenticationRetryTask?.cancel()
        accountEnrollmentTask?.cancel()
        changesSubscription?.cancel()
        pathMonitor.cancel()
    }

    func createSection(name: String) {
        Task {
            do {
                guard let store else { throw LocalSyncStoreError.storageUnavailable }
                _ = try await store.createSection(name: name)
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The section could not be saved."
            }
        }
    }

    func renameSection(id: UUID, name: String) {
        Task {
            do {
                guard let store else { throw LocalSyncStoreError.storageUnavailable }
                try await store.renameSection(id: id, name: name)
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The section could not be renamed."
            }
        }
    }

    func deleteSection(id: UUID) {
        Task {
            do {
                guard let store else { throw LocalSyncStoreError.storageUnavailable }
                try await store.deleteSection(id: id)
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The section could not be deleted."
            }
        }
    }

    func createItem(sectionId: UUID, content: String) {
        Task {
            do {
                guard let store else { throw LocalSyncStoreError.storageUnavailable }
                _ = try await store.createItem(sectionId: sectionId, content: content)
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The item could not be saved."
            }
        }
    }

    func updateItem(id: UUID, content: String) {
        Task {
            do {
                guard let store else { throw LocalSyncStoreError.storageUnavailable }
                try await store.updateItem(id: id, content: content)
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The item could not be updated."
            }
        }
    }

    func resolveItemConflict(id: UUID, content: String) {
        Task {
            do {
                guard let store else { throw LocalSyncStoreError.storageUnavailable }
                try await store.resolveItemConflict(id: id, content: content)
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The conflict could not be resolved."
            }
        }
    }

    func setItemCompleted(id: UUID, done: Bool) {
        Task {
            do {
                guard let store else { throw LocalSyncStoreError.storageUnavailable }
                try await store.setItemCompleted(id: id, done: done)
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The item could not be updated."
            }
        }
    }

    func deleteItem(id: UUID) {
        Task {
            do {
                guard let store else { throw LocalSyncStoreError.storageUnavailable }
                try await store.deleteItem(id: id)
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The item could not be deleted."
            }
        }
    }

    func addAttachment(itemId: UUID, url: URL) {
        Task {
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            do {
                guard let store, let workspaceId else {
                    throw LocalSyncStoreError.storageUnavailable
                }
                let values = try url.resourceValues(forKeys: [.fileSizeKey])
                guard let fileSize = values.fileSize,
                      fileSize >= 0,
                      UInt64(fileSize) <= LocalSyncStore.maxAttachmentBytes else {
                    throw LocalSyncStoreError.invalidManifest
                }
                let key = try workspaceKey(workspaceId: workspaceId)
                let mediaType = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
                    ?? "application/octet-stream"
                _ = try await store.addAttachment(
                    itemId: itemId,
                    name: url.lastPathComponent,
                    mediaType: mediaType,
                    fileURL: url,
                    key: key
                )
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The attachment could not be imported."
            }
        }
    }

    func addAttachment(itemId: UUID, name: String, mediaType: String, data: Data) {
        Task {
            do {
                guard let store, let workspaceId else {
                    throw LocalSyncStoreError.storageUnavailable
                }
                guard UInt64(data.count) <= LocalSyncStore.maxAttachmentBytes else {
                    throw LocalSyncStoreError.invalidManifest
                }
                let key = try workspaceKey(workspaceId: workspaceId)
                _ = try await store.addAttachment(
                    itemId: itemId,
                    name: name,
                    mediaType: mediaType,
                    data: data,
                    key: key
                )
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The attachment could not be imported."
            }
        }
    }

    func deleteAttachment(id: UUID) {
        Task {
            do {
                guard let store else { throw LocalSyncStoreError.storageUnavailable }
                try await store.deleteAttachment(id: id)
                await refreshLibrary()
                wakeSync()
            } catch {
                message = "The attachment could not be deleted."
            }
        }
    }

    func syncNow() { wakeSync() }

    func signOut() {
        auth.signOut()
        cloudAuthenticated = false
        stopForegroundNetworking()
        Task { [cloud] in await cloud.signOut() }
    }

    func sceneChanged(_ phase: UIScene.ActivationState) {
        isForeground = phase == .foregroundActive
        if isForeground {
            NotificationCenter.default.post(name: .clippySyncForegrounded, object: nil)
            ensureCloudAuthentication()
        } else {
            stopForegroundNetworking()
        }
    }

    private func networkPathChanged(_ online: Bool) {
        guard isOnline != online else { return }
        isOnline = online
        if online {
            ensureCloudAuthentication()
        } else {
            stopForegroundNetworking()
            syncState = workspaceId == nil ? .idle : .waitingForDevice
        }
    }

    private func openReplica(workspaceId: String) async {
        do {
            try await installReplica(workspaceId: workspaceId)
            ensureCloudAuthentication()
        } catch {
            syncState = .waitingForDevice
            message = "Local sync data could not be opened."
        }
    }

    private func installReplica(workspaceId: String) async throws {
        let actorId = deviceActorId
        let replica = try await Task.detached(priority: .userInitiated) {
            try LocalSyncStore(workspaceId: workspaceId, actorId: actorId)
        }.value
        confirmedRemoteChunks.removeAll(keepingCapacity: true)
        store = replica
        library = await replica.view()
        deviceActorId = library.actorId
        try? keychain.save(
            Data(deviceActorId.utf8),
            account: configuration.keychainAccount("device-actor")
        )
    }

    private func refreshLibrary() async {
        guard let store else {
            library = .empty
            return
        }
        library = await store.view()
    }

    private func ensureCloudAuthentication() {
        guard auth.signedIn, isForeground, isOnline else { return }
        if cloudAuthenticated {
            startForegroundNetworking()
            return
        }
        guard cloudAuthenticationTask == nil, cloudAuthenticationRetryTask == nil else { return }
        cloudAuthenticationTask = Task { [weak self] in
            guard let self else { return }
            do {
                try await cloud.authenticate()
                try Task.checkCancellation()
                guard auth.signedIn, isForeground, isOnline else {
                    cloudAuthenticationTask = nil
                    return
                }
                cloudAuthenticated = true
                authenticationRetryIndex = 0
                do { _ = try await resumePendingEnrollmentAcceptance() }
                catch { message = "Waiting to finish secure account enrollment." }
                cloudAuthenticationTask = nil
                startForegroundNetworking()
            } catch is CancellationError {
                cloudAuthenticationTask = nil
            } catch {
                cloudAuthenticated = false
                cloudAuthenticationTask = nil
                message = "Sync sign-in could not be verified by Convex. Clippy will retry."
                scheduleCloudAuthenticationRetry()
            }
        }
    }

    private func scheduleCloudAuthenticationRetry() {
        guard cloudAuthenticationRetryTask == nil, auth.signedIn, isForeground, isOnline else {
            return
        }
        let ladder: [TimeInterval] = [1, 2, 4, 8, 16, 60]
        let delay = ladder[min(authenticationRetryIndex, ladder.count - 1)]
        authenticationRetryIndex = min(authenticationRetryIndex + 1, ladder.count - 1)
        cloudAuthenticationRetryTask = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(delay)) }
            catch { return }
            guard let self else { return }
            cloudAuthenticationRetryTask = nil
            ensureCloudAuthentication()
        }
    }

    /// Coalesces Convex change hints and local writes. A write arriving during
    /// an exchange runs once more immediately instead of starting a parallel
    /// mutation or cancelling an ambiguous upload.
    private func wakeSync() {
        guard isForeground, isOnline, store != nil, workspaceId != nil,
              cloudAuthenticated, auth.signedIn else { return }
        syncRequested = true
        guard exchangeTask == nil else { return }
        let generation = UUID()
        exchangeGeneration = generation
        exchangeTask = Task { [weak self] in
            guard let self else { return }
            var exchangeCount = 0
            while !Task.isCancelled, self.isForeground, self.syncRequested,
                  exchangeCount < 8 {
                self.syncRequested = false
                exchangeCount += 1
                guard await self.performSyncOnce() else { break }
            }
            // A corrupt or non-advancing batch must not create an unbounded
            // foreground request loop. Durable work is woken by Convex or the
            // five-minute safety pass.
            if exchangeCount == 8 { self.syncRequested = false }
            if self.exchangeGeneration == generation { self.exchangeTask = nil }
        }
    }

    private func startForegroundNetworking() {
        guard isForeground, isOnline, auth.signedIn, cloudAuthenticated else { return }
        guard let store, let workspaceId else {
            beginAutomaticAccountEnrollment()
            return
        }
        if changesSubscription == nil {
            let actorId = library.actorId
            changesSubscription = cloud
                .changes(workspaceId: workspaceId, actorId: actorId)
                .receive(on: RunLoop.main)
                .sink(
                    receiveCompletion: { [weak self] completion in
                        guard let self else { return }
                        self.changesSubscription = nil
                        if case .failure = completion { self.scheduleTransientSyncRetry() }
                    },
                    receiveValue: { [weak self] counters in
                        guard let self else { return }
                        self.cloudCounters = Dictionary(uniqueKeysWithValues: counters.compactMap {
                            guard $0.latestCounter >= 0,
                                  $0.latestCounter <= 9_007_199_254_740_991,
                                  $0.latestCounter.rounded() == $0.latestCounter else { return nil }
                            return ($0.actorId, UInt64($0.latestCounter))
                        })
                        self.wakeSync()
                    }
                )
        }
        _ = store
        wakeSync()
        if safetyPollTask == nil {
            safetyPollTask = Task { [weak self] in
                while let self, !Task.isCancelled, self.isForeground, self.isOnline {
                    do { try await Task.sleep(for: .seconds(300)) }
                    catch { return }
                    self.wakeSync()
                }
            }
        }
    }

    private func stopForegroundNetworking() {
        syncRequested = false
        exchangeGeneration = UUID()
        safetyPollTask?.cancel()
        safetyPollTask = nil
        changesSubscription?.cancel()
        changesSubscription = nil
        exchangeTask?.cancel()
        exchangeTask = nil
        transientSyncRetryTask?.cancel()
        transientSyncRetryTask = nil
        accountEnrollmentTask?.cancel()
        accountEnrollmentTask = nil
        cloudAuthenticationTask?.cancel()
        cloudAuthenticationTask = nil
        cloudAuthenticationRetryTask?.cancel()
        cloudAuthenticationRetryTask = nil
        connectingAccount = false
    }

    private func performSyncOnce() async -> Bool {
        guard !exchangeInProgress else { return true }
        guard let store, let workspaceId, auth.signedIn else {
            syncState = workspaceId == nil ? .idle : .waitingForDevice
            return false
        }
        exchangeInProgress = true
        syncState = .syncing
        defer { exchangeInProgress = false }

        do {
            let key = try workspaceKey(workspaceId: workspaceId)

            // R2 objects are uploaded before the operation containing their
            // manifest. Confirmed hashes are memoized for this foreground run.
            let moreUploads = try await uploadMissingChunks(store: store, workspaceId: workspaceId)
            if moreUploads {
                syncRequested = true
                return true
            }

            let actorId = library.actorId
            if let accepted = cloudCounters[actorId], accepted > 0 {
                _ = try await store.applyRemotePayload(
                    SyncPayload(
                        workspaceId: workspaceId,
                        frontier: VersionVector([actorId: accepted]),
                        operations: []
                    )
                )
            }

            var limit = 256
            var outbound = try await store.outboundPayload(limit: limit)
            var outboundMayHaveMore = false
            while !outbound.operations.isEmpty {
                let encoded = try JSONEncoder().encode(outbound)
                if encoded.count <= 550_000 {
                    let first = outbound.operations.first!.dot.counter
                    let last = outbound.operations.last!.dot.counter
                    let batch = CloudBatch(
                        actorId: actorId,
                        firstCounter: first,
                        lastCounter: last,
                        envelope: try SyncCrypto.seal(
                            encoded,
                            key: key,
                            aad: SyncCrypto.batchAAD(
                                workspaceId: workspaceId,
                                actorId: actorId,
                                firstCounter: first,
                                lastCounter: last
                            )
                        )
                    )
                    let accepted = try await cloud.push(workspaceId: workspaceId, batch: batch)
                    guard accepted >= last else { throw ConvexSyncError.invalidResponse }
                    cloudCounters[actorId] = accepted
                    _ = try await store.applyRemotePayload(
                        SyncPayload(
                            workspaceId: workspaceId,
                            frontier: VersionVector([actorId: accepted]),
                            operations: []
                        )
                    )
                    outboundMayHaveMore = outbound.operations.count == limit
                    break
                }
                guard limit > 1 else { throw ConvexSyncError.invalidResponse }
                limit = max(1, limit / 2)
                outbound = try await store.outboundPayload(limit: limit)
            }

            let frontier = await store.frontier()
            let remoteIsAhead = cloudCounters.contains { actor, counter in
                counter > frontier.counters[actor, default: 0]
            }
            let batches: [CloudBatch]
            if remoteIsAhead {
                batches = try await cloud.pull(
                    workspaceId: workspaceId,
                    actorId: actorId,
                    frontier: frontier
                )
            } else {
                batches = []
            }
            for batch in batches {
                let plaintext = try SyncCrypto.open(
                    batch.envelope,
                    key: key,
                    aad: SyncCrypto.batchAAD(
                        workspaceId: workspaceId,
                        actorId: batch.actorId,
                        firstCounter: batch.firstCounter,
                        lastCounter: batch.lastCounter
                    )
                )
                guard plaintext.count <= 550_000 else { throw ConvexSyncError.invalidResponse }
                let incoming = try JSONDecoder().decode(SyncPayload.self, from: plaintext)
                guard incoming.operations.first?.dot.counter == batch.firstCounter,
                      incoming.operations.last?.dot.counter == batch.lastCounter,
                      incoming.operations.allSatisfy({ $0.dot.actorId == batch.actorId }) else {
                    throw ConvexSyncError.invalidResponse
                }
                _ = try await store.applyRemotePayload(incoming)
            }

            let missingBeforeDownload = await store.missingChunkHashes()
            let missingBatch = Array(missingBeforeDownload.prefix(64))
            if !missingBatch.isEmpty {
                let downloads = try await cloud.downloadURLs(
                    workspaceId: workspaceId,
                    hashes: missingBatch
                )
                for download in downloads {
                    guard let url = URL(string: download.url) else {
                        throw ConvexSyncError.invalidResponse
                    }
                    let chunk = try await cloud.download(from: url)
                    try await store.saveRemoteChunk(hash: download.hash, envelope: chunk, key: key)
                    confirmedRemoteChunks.insert(download.hash)
                }
            }
            await refreshLibrary()
            let hasMissingDownloads = !(await store.missingChunkHashes()).isEmpty
            let hasMore = moreUploads ||
                batches.count == 12 ||
                outboundMayHaveMore ||
                library.pendingOperationCount > 0 ||
                hasMissingDownloads
            syncRequested = syncRequested || hasMore
            syncState = .synced
            retryIndex = 0
            return true
        } catch {
            scheduleTransientSyncRetry()
            syncState = .waitingForDevice
            return false
        }
    }

    private func scheduleTransientSyncRetry() {
        guard transientSyncRetryTask == nil, isOnline, isForeground else { return }
        let ladder: [TimeInterval] = [1, 2, 4, 8, 16]
        let delay = ladder[min(retryIndex, ladder.count - 1)]
        retryIndex = min(retryIndex + 1, ladder.count - 1)
        transientSyncRetryTask = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(delay)) }
            catch { return }
            guard let self, self.isOnline, self.isForeground else { return }
            self.transientSyncRetryTask = nil
            self.wakeSync()
        }
    }

    /// One Convex action checks at most 64 R2 objects and returns short-lived
    /// upload URLs only for missing hashes. Bytes go straight to R2.
    private func uploadMissingChunks(
        store: LocalSyncStore,
        workspaceId: String
    ) async throws -> Bool {
        let candidates = await store.pendingChunkHashes().filter {
            !confirmedRemoteChunks.contains($0)
        }
        guard !candidates.isEmpty else { return false }

        let batch = Array(candidates.prefix(64))
        let uploads = try await cloud.prepareUploads(workspaceId: workspaceId, hashes: batch)
        guard Set(uploads.map(\.hash)) == Set(batch) else {
            throw ConvexSyncError.invalidResponse
        }
        for upload in uploads {
            if !upload.exists {
                guard let encoded = upload.url, let url = URL(string: encoded) else {
                    throw ConvexSyncError.invalidResponse
                }
                try await cloud.upload(try await store.sealedChunk(hash: upload.hash), to: url)
            }
            confirmedRemoteChunks.insert(upload.hash)
        }
        return candidates.count > batch.count
    }

    private func workspaceKey(workspaceId: String) throws -> WorkspaceKey {
        guard let data = try keychain.load(
            account: configuration.keychainAccount("workspace-key:\(workspaceId)")
        ) else {
            throw SyncCryptoError.invalidKey
        }
        return try WorkspaceKey(data: data)
    }

    private func beginAutomaticAccountEnrollment() {
        guard accountEnrollmentTask == nil else { return }
        connectingAccount = true
        accountEnrollmentTask = Task { [weak self] in
            guard let self else { return }
            var retryDelay: TimeInterval = 0
            while !Task.isCancelled, self.isForeground, self.isOnline, self.auth.signedIn,
                  (self.workspaceId == nil || self.store == nil) {
                if retryDelay > 0 {
                    do { try await Task.sleep(for: .seconds(retryDelay)) }
                    catch { break }
                }
                do {
                    try await self.enrollSignedInAccount()
                    break
                } catch AccountEnrollmentError.noEnvironment {
                    self.syncState = .waitingForDevice
                    self.message = "Open Clippy on your Mac and sign in there. This iPhone will connect automatically."
                } catch {
                    self.syncState = .waitingForDevice
                    self.message = "Waiting for secure account enrollment."
                }
                retryDelay = retryDelay == 0 ? 1 : min(retryDelay * 2, 30)
            }
            self.connectingAccount = false
            self.accountEnrollmentTask = nil
        }
    }

    private func enrollSignedInAccount() async throws {
        if try await resumePendingEnrollmentAcceptance() { return }
        syncState = .syncing
        let enrollment = SyncCrypto.AccountEnrollment()
        let enrollmentId = UUID().uuidString.lowercased()
        let requested = try await cloud.requestEnrollment(
            enrollmentId: enrollmentId,
            actorId: deviceActorId,
            deviceName: "Clippy on this iPhone",
            phonePublicKey: enrollment.request.phonePublicKey
        )
        guard requested.state != "noWorkspace" else {
            throw AccountEnrollmentError.noEnvironment
        }
        guard requested.state != "alreadyEnrolled" else {
            throw AccountEnrollmentError.invalidResponse
        }

        var response: AccountEnrollmentResponse?
        var delay: TimeInterval = 1
        while !Task.isCancelled, response == nil {
            let status = try await cloud.enrollmentStatus(
                enrollmentId: enrollmentId,
                actorId: deviceActorId
            )
            if status?.state == "granted" {
                response = status?.response
                break
            }
            if status?.state == "expired" { throw AccountEnrollmentError.invalidResponse }
            try await Task.sleep(for: .seconds(delay))
            delay = min(delay * 2, 15)
        }
        guard let response else { throw CancellationError() }
        let offer = response.offer
        guard offer.workspaceId == requested.workspaceId,
              offer.workosIssuer == configuration.workOSIssuer.absoluteString,
              offer.workosAudience == configuration.workOSAudience,
              offer.syncUrl.trimmingCharacters(in: CharacterSet(charactersIn: "/")) ==
                configuration.convexURL.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/")),
              UInt64(Date().timeIntervalSince1970 * 1_000) <= offer.expiresAtMs else {
            throw AccountEnrollmentError.invalidResponse
        }
        let key = try enrollment.unwrap(
            response: response,
            principal: try await auth.principal()
        )
        try keychain.save(
            key.data,
            account: configuration.keychainAccount("workspace-key:\(offer.workspaceId)")
        )
        try keychain.save(
            Data(offer.workspaceId.utf8),
            account: configuration.keychainAccount("workspace-id")
        )
        try keychain.save(
            Data(enrollmentId.utf8),
            account: configuration.keychainAccount("pending-enrollment-id")
        )
        try await finishEnrollmentAcceptance(
            enrollmentId: enrollmentId,
            workspaceId: offer.workspaceId
        )
    }

    private func resumePendingEnrollmentAcceptance() async throws -> Bool {
        guard let enrollmentData = try keychain.load(
            account: configuration.keychainAccount("pending-enrollment-id")
        ), let enrollmentId = String(data: enrollmentData, encoding: .utf8),
           let workspaceData = try keychain.load(
            account: configuration.keychainAccount("workspace-id")
           ), let workspace = String(data: workspaceData, encoding: .utf8),
           UUID(uuidString: enrollmentId) != nil, UUID(uuidString: workspace) != nil else {
            return false
        }
        try await finishEnrollmentAcceptance(enrollmentId: enrollmentId, workspaceId: workspace)
        return true
    }

    private func finishEnrollmentAcceptance(
        enrollmentId: String,
        workspaceId acceptedWorkspaceId: String
    ) async throws {
        let acceptedWorkspace = try await cloud.acceptEnrollment(
            enrollmentId: enrollmentId,
            actorId: deviceActorId
        )
        guard acceptedWorkspace == acceptedWorkspaceId else {
            throw AccountEnrollmentError.invalidResponse
        }
        try keychain.delete(account: configuration.keychainAccount("pending-enrollment-id"))
        workspaceId = acceptedWorkspaceId
        if store == nil {
            try await installReplica(workspaceId: acceptedWorkspaceId)
        }
        syncState = .synced
        message = nil
        startForegroundNetworking()
    }
}

private enum AccountEnrollmentError: Error { case noEnvironment, invalidResponse }

extension Notification.Name {
    static let clippySyncForegrounded = Notification.Name("clippy-sync-foregrounded")
}
