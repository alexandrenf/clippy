import Combine
import Foundation
import Network
import UniformTypeIdentifiers
import UIKit

@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var syncState: SyncState = .idle
    @Published private(set) var library: LocalSyncView = .empty
    @Published private(set) var needsRelayPairing = false
    @Published var pairingCode = ""
    @Published var message: String?

    let auth: AuthController
    private let configuration: RuntimeConfiguration
    private let keychain = KeychainStore()
    private var workspaceId: String?
    private var environmentId: String?
    private var store: LocalSyncStore?
    private var dpopSigner: DPoPSigner?
    private var relayClient: RelayClient?
    private var environmentSession: EnvironmentSession?
    private var environmentConnectedAt: Date?
    private var transport: SyncTransport?
    private var connectionTask: Task<SyncTransport, Error>?
    private var transientSyncRetryTask: Task<Void, Never>?
    private var supervisor = ConnectionSupervisor()
    private var safetyPollTask: Task<Void, Never>?
    private var eventLoopTask: Task<Void, Never>?
    private var exchangeTask: Task<Void, Never>?
    private var eventSocket: URLSessionWebSocketTask?
    private var authSubscription: AnyCancellable?
    private let pathMonitor = NWPathMonitor()
    private let pathQueue = DispatchQueue(label: "app.clippy.mobile.relay-path")
    private var isForeground = true
    private var isOnline = true
    private var exchangeInProgress = false
    private var syncRequested = false
    private var exchangeGeneration = UUID()
    private var eventLoopGeneration = UUID()
    private var confirmedRemoteChunks: Set<String> = []

    init(configuration: RuntimeConfiguration) {
        self.configuration = configuration
        auth = AuthController(configuration: configuration)
        if let data = try? keychain.load(account: configuration.keychainAccount("environment-id")),
           let value = String(data: data, encoding: .utf8),
           !value.isEmpty {
            environmentId = value
        }
        if let data = try? keychain.load(account: configuration.keychainAccount("workspace-id")),
           let workspace = String(data: data, encoding: .utf8),
           !workspace.isEmpty {
            workspaceId = workspace
            needsRelayPairing = environmentId == nil
            syncState = .waitingForDevice
            Task { [weak self] in await self?.openReplica(workspaceId: workspace) }
        }
        authSubscription = auth.$signedIn
            .removeDuplicates()
            .sink { [weak self] signedIn in
                Task { @MainActor in
                    if signedIn {
                        _ = self?.supervisor.credentialOrConfigurationWake(at: Date())
                        self?.startForegroundNetworking()
                    }
                    else { self?.stopForegroundNetworking() }
                }
            }
        pathMonitor.pathUpdateHandler = { [weak self] path in
            Task { @MainActor in self?.networkPathChanged(path.status == .satisfied) }
        }
        pathMonitor.start(queue: pathQueue)
    }

    deinit {
        safetyPollTask?.cancel()
        eventLoopTask?.cancel()
        exchangeTask?.cancel()
        eventSocket?.cancel(with: .goingAway, reason: nil)
        connectionTask?.cancel()
        transientSyncRetryTask?.cancel()
        pathMonitor.cancel()
    }

    func pair() {
        Task {
            do {
                syncState = .syncing
                let offer = try Self.decodeOffer(pairingCode)
                guard offer.workosIssuer == configuration.workOSIssuer.absoluteString,
                      offer.workosAudience == configuration.workOSClientID else {
                    throw PairingError.environmentMismatch
                }
                guard UInt64(Date().timeIntervalSince1970 * 1_000) <= offer.expiresAtMs else {
                    throw PairingError.expired
                }
                let phone = SyncCrypto.PhonePairing(offer: offer)
                let accessToken = try await auth.accessToken()
                let relay = try relay()
                let environments = try await relay.environments(workOSAccessToken: accessToken)
                let selected = try Self.selectEnvironment(
                    for: offer.workspaceId,
                    from: environments
                )
                let connection = try await relay.connect(
                    environmentId: selected.id,
                    workOSAccessToken: accessToken
                )
                let session = try await relay.bootstrap(connection)
                let transport = try SyncTransport(
                    environment: session,
                    signer: try signer()
                )
                let grant = try await transport.pair(response: phone.response)
                let key = try phone.unwrap(
                    grant: grant,
                    offer: offer,
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
                try persist(environment: session)
                workspaceId = offer.workspaceId
                environmentId = selected.id
                needsRelayPairing = false
                environmentSession = session
                environmentConnectedAt = Date()
                self.transport = transport
                supervisor.connected(leaseExpiresAt: session.expiresAt, at: Date())
                pairingCode = ""
                try await installReplica(workspaceId: offer.workspaceId)
                syncState = .synced
                message = "This iPhone is paired."
                startForegroundNetworking()
            } catch {
                syncState = .waitingForDevice
                message = "Pairing could not be verified."
            }
        }
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

    func sceneChanged(_ phase: UIScene.ActivationState) {
        isForeground = phase == .foregroundActive
        if isForeground {
            NotificationCenter.default.post(name: .clippySyncForegrounded, object: nil)
            let command = supervisor.foregrounded(at: Date())
            if command == .replaceConnection { invalidateEnvironmentSession() }
            startForegroundNetworking()
        } else {
            _ = supervisor.backgrounded(at: Date())
            stopForegroundNetworking()
        }
    }

    private func networkPathChanged(_ online: Bool) {
        guard isOnline != online else { return }
        isOnline = online
        let command = supervisor.setOnline(online, at: Date())
        if online {
            if command == .replaceConnection { invalidateEnvironmentSession() }
            startForegroundNetworking()
        } else {
            stopForegroundNetworking()
            syncState = workspaceId == nil ? .idle : .waitingForDevice
        }
    }

    private func openReplica(workspaceId: String) async {
        do {
            try await installReplica(workspaceId: workspaceId)
            startForegroundNetworking()
        } catch {
            syncState = .waitingForDevice
            message = "Local sync data could not be opened."
        }
    }

    private func installReplica(workspaceId: String) async throws {
        let replica = try await Task.detached(priority: .userInitiated) {
            try LocalSyncStore(workspaceId: workspaceId)
        }.value
        confirmedRemoteChunks.removeAll(keepingCapacity: true)
        store = replica
        library = await replica.view()
    }

    private func refreshLibrary() async {
        guard let store else {
            library = .empty
            return
        }
        library = await store.view()
    }

    /// Coalesces hints and local changes. A write arriving during an exchange
    /// runs once more immediately after that exchange instead of cancelling an
    /// ambiguous authenticated POST or starting parallel requests.
    private func wakeSync() {
        guard isForeground, isOnline, store != nil, workspaceId != nil,
              environmentId != nil, auth.signedIn else { return }
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
            // A corrupt or non-advancing peer must not create an unbounded
            // foreground request loop. Remaining durable work will be woken by
            // the socket or the five-minute safety poll.
            if exchangeCount == 8 { self.syncRequested = false }
            if self.exchangeGeneration == generation { self.exchangeTask = nil }
        }
    }

    private func startForegroundNetworking() {
        guard isForeground, isOnline, store != nil, workspaceId != nil,
              environmentId != nil, auth.signedIn else { return }
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
        if eventLoopTask == nil {
            let generation = UUID()
            eventLoopGeneration = generation
            eventLoopTask = Task { [weak self] in
                await self?.runEventSocketLoop(generation: generation)
                if self?.eventLoopGeneration == generation { self?.eventLoopTask = nil }
            }
        }
    }

    private func stopForegroundNetworking() {
        syncRequested = false
        exchangeGeneration = UUID()
        eventLoopGeneration = UUID()
        safetyPollTask?.cancel()
        safetyPollTask = nil
        eventLoopTask?.cancel()
        eventLoopTask = nil
        exchangeTask?.cancel()
        exchangeTask = nil
        connectionTask?.cancel()
        connectionTask = nil
        transientSyncRetryTask?.cancel()
        transientSyncRetryTask = nil
        eventSocket?.cancel(with: .goingAway, reason: nil)
        eventSocket = nil
    }

    /// One ticket-authenticated socket carries state-free hints. Long-lived
    /// environment credentials never enter the upgrade. The supervisor owns
    /// the 1/2/4/8/16-second transient ladder; offline and blocked states own no
    /// timer at all.
    private func runEventSocketLoop(generation: UUID) async {
        var currentSocket: URLSessionWebSocketTask?
        while !Task.isCancelled, isForeground, isOnline, auth.signedIn,
              eventLoopGeneration == generation {
            do {
                let transport = try await ensureTransport()
                let socket = try await transport.eventSocket()
                socket.maximumMessageSize = 1_024
                currentSocket = socket
                eventSocket = socket
                socket.resume()
                // The origin intentionally sends no initial event. Drain any
                // operation or chunk that failed before this connection.
                wakeSync()
                while !Task.isCancelled, isForeground, eventLoopGeneration == generation {
                    let message = try await socket.receive()
                    switch message {
                    case let .string(value):
                        guard value.utf8.count <= 1_024 else { throw TransportError.invalidResponse }
                    case let .data(value):
                        guard value.count <= 1_024 else { throw TransportError.invalidResponse }
                    @unknown default:
                        throw TransportError.invalidResponse
                    }
                    wakeSync()
                }
            } catch is CancellationError {
                currentSocket?.cancel(with: .goingAway, reason: nil)
                return
            } catch {
                currentSocket?.cancel(with: .goingAway, reason: nil)
                currentSocket = nil
                if eventLoopGeneration == generation { eventSocket = nil }
                if shouldBlockForCredentialOrConfiguration(error) { return }
                if isLeaseRejection(error) { invalidateEnvironmentSession() }
                let command = supervisor.transientFailure(at: Date())
                guard case let .connect(after: delay) = command else { return }
                do { try await Task.sleep(for: .seconds(delay)) }
                catch { return }
            }
        }
        currentSocket?.cancel(with: .goingAway, reason: nil)
        if eventLoopGeneration == generation { eventSocket = nil }
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
            let transport = try await ensureTransport()

            // Chunks are uploaded before their manifest operation. Confirmed
            // content hashes are remembered for this process so a steady-state
            // safety poll does not re-probe every attachment.
            let moreUploads = try await uploadMissingChunks(
                store: store,
                transport: transport
            )
            if moreUploads {
                syncRequested = true
                return true
            }

            let payload = try await store.outboundPayload()
            let envelope = try SyncCrypto.seal(
                JSONEncoder().encode(payload),
                key: key,
                aad: SyncCrypto.payloadAAD(workspaceId: workspaceId, schemaVersion: payload.schemaVersion)
            )
            let response = try await transport.exchange(
                envelope: envelope,
                deviceId: library.actorId
            )
            let plaintext = try SyncCrypto.open(
                response,
                key: key,
                aad: SyncCrypto.payloadAAD(workspaceId: workspaceId)
            )
            let incoming = try JSONDecoder().decode(SyncPayload.self, from: plaintext)
            _ = try await store.applyRemotePayload(incoming)

            let missingBeforeDownload = await store.missingChunkHashes()
            for hash in missingBeforeDownload.prefix(256) {
                let chunk = try await transport.downloadChunk(hash: hash)
                try await store.saveRemoteChunk(hash: hash, envelope: chunk, key: key)
                confirmedRemoteChunks.insert(hash)
            }
            await refreshLibrary()
            let hasMissingDownloads = !(await store.missingChunkHashes()).isEmpty
            let hasMore = moreUploads ||
                incoming.operations.count == 2_000 ||
                library.pendingOperationCount > 0 ||
                hasMissingDownloads
            syncRequested = syncRequested || hasMore
            syncState = .synced
            return true
        } catch {
            if isLeaseRejection(error) { invalidateEnvironmentSession() }
            if !shouldBlockForCredentialOrConfiguration(error) {
                scheduleTransientSyncRetry()
            }
            syncState = .waitingForDevice
            return false
        }
    }

    private func scheduleTransientSyncRetry() {
        guard transientSyncRetryTask == nil, isOnline, isForeground else { return }
        let command = supervisor.transientFailure(at: Date())
        guard case let .connect(after: delay) = command else { return }
        transientSyncRetryTask = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(delay)) }
            catch { return }
            guard let self, self.isOnline, self.isForeground else { return }
            self.transientSyncRetryTask = nil
            self.wakeSync()
        }
    }

    /// Probes at most 4,096 not-yet-confirmed hashes and uploads at most 256
    /// chunks per exchange. Successful probes are memoized, so pagination
    /// advances instead of repeatedly starting from the same hash batch.
    private func uploadMissingChunks(
        store: LocalSyncStore,
        transport: SyncTransport
    ) async throws -> Bool {
        let candidates = await store.availableChunkHashes().filter {
            !confirmedRemoteChunks.contains($0)
        }
        guard !candidates.isEmpty else { return false }

        let batchSize = 256
        let maxBatches = 16
        var uploadBudget = 256
        var processed = 0
        for start in stride(from: 0, to: candidates.count, by: batchSize).prefix(maxBatches) {
            let end = min(start + batchSize, candidates.count)
            let batch = Array(candidates[start..<end])
            let missing = try await transport.missingChunks(hashes: batch)
            guard missing.isSubset(of: Set(batch)) else { throw TransportError.invalidResponse }
            confirmedRemoteChunks.formUnion(batch.filter { !missing.contains($0) })

            for hash in batch where missing.contains(hash) && uploadBudget > 0 {
                try await transport.uploadChunk(
                    hash: hash,
                    sealedChunk: try await store.sealedChunk(hash: hash)
                )
                confirmedRemoteChunks.insert(hash)
                uploadBudget -= 1
            }
            processed = end
            if uploadBudget == 0 { break }
        }
        return processed < candidates.count || candidates.contains {
            !confirmedRemoteChunks.contains($0)
        }
    }

    private func ensureTransport() async throws -> SyncTransport {
        guard let environmentId else { throw AppConnectionError.missingEnvironment }
        if let session = environmentSession,
           let transport,
           session.environmentId == environmentId,
           session.isHealthy(at: Date()) {
            supervisor.connected(
                leaseExpiresAt: session.expiresAt,
                at: environmentConnectedAt ?? Date()
            )
            return transport
        }
        if let connectionTask { return try await connectionTask.value }

        let task = Task { @MainActor [weak self] () throws -> SyncTransport in
            guard let self else { throw CancellationError() }
            return try await self.connectEnvironment(environmentId: environmentId)
        }
        connectionTask = task
        do {
            let transport = try await task.value
            connectionTask = nil
            return transport
        } catch {
            connectionTask = nil
            throw error
        }
    }

    private func connectEnvironment(environmentId: String) async throws -> SyncTransport {
        let workOSToken: String
        do { workOSToken = try await auth.accessToken() }
        catch { throw AppConnectionError.authentication }

        let relay = try relay()
        let connection: EnvironmentConnection
        do {
            connection = try await relay.connect(
                environmentId: environmentId,
                workOSAccessToken: workOSToken
            )
        } catch RelayError.http(let status) where status == 401 || status == 403 {
            throw AppConnectionError.authentication
        }
        let session = try await relay.bootstrap(connection)
        let transport = try SyncTransport(environment: session, signer: try signer())
        try persist(environment: session)
        environmentSession = session
        environmentConnectedAt = Date()
        self.transport = transport
        supervisor.connected(leaseExpiresAt: session.expiresAt, at: Date())
        return transport
    }

    private func signer() throws -> DPoPSigner {
        if let dpopSigner { return dpopSigner }
        let loaded = try DPoPSigner.loadOrCreate(
            keychain: keychain,
            account: configuration.keychainAccount("relay-dpop-p256-key")
        )
        dpopSigner = loaded
        return loaded
    }

    private func relay() throws -> RelayClient {
        if let relayClient { return relayClient }
        let client = try RelayClient(
            baseURL: configuration.relayBaseURL,
            endpointHostSuffix: configuration.syncEndpointHostSuffix,
            signer: try signer()
        )
        relayClient = client
        return client
    }

    private func persist(environment: EnvironmentSession) throws {
        try keychain.save(
            Data(environment.environmentId.utf8),
            account: configuration.keychainAccount("environment-id")
        )
        try keychain.save(
            Data(environment.endpoint.httpBaseURL.absoluteString.utf8),
            account: configuration.keychainAccount("environment-http-url")
        )
        try keychain.save(
            Data(environment.endpoint.wsBaseURL.absoluteString.utf8),
            account: configuration.keychainAccount("environment-ws-url")
        )
    }

    private func invalidateEnvironmentSession() {
        environmentSession = nil
        environmentConnectedAt = nil
        transport = nil
        connectionTask?.cancel()
        connectionTask = nil
        supervisor.invalidateLease()
    }

    @discardableResult
    private func shouldBlockForCredentialOrConfiguration(_ error: Error) -> Bool {
        if case AppConnectionError.authentication = error {
            supervisor.block(.authentication)
            message = "Sign in again to reconnect sync."
            auth.signOut()
            return true
        }
        switch error {
        case RelayError.invalidConfiguration,
             RelayError.invalidEnvironment,
             RelayError.keyBindingMismatch,
             RelayError.untrustedEndpoint:
            supervisor.block(.configuration)
            message = "Sync relay configuration could not be verified."
            return true
        default:
            return false
        }
    }

    private func isLeaseRejection(_ error: Error) -> Bool {
        switch error {
        case TransportError.expiredLease,
             TransportError.http(401),
             TransportError.http(403):
            return true
        default:
            return false
        }
    }

    private func workspaceKey(workspaceId: String) throws -> WorkspaceKey {
        guard let data = try keychain.load(
            account: configuration.keychainAccount("workspace-key:\(workspaceId)")
        ) else {
            throw SyncCryptoError.invalidKey
        }
        return try WorkspaceKey(data: data)
    }

    private static func decodeOffer(_ value: String) throws -> PairingOffer {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        let data: Data
        if trimmed.first == "{" { data = Data(trimmed.utf8) }
        else { data = try Data(base64URLEncoded: trimmed) }
        return try JSONDecoder().decode(PairingOffer.self, from: data)
    }

    private static func selectEnvironment(
        for workspaceId: String,
        from environments: [RelayEnvironment]
    ) throws -> RelayEnvironment {
        let matching = environments.filter {
            $0.workspaceId == workspaceId || $0.id == workspaceId
        }
        if matching.count == 1 { return matching[0] }
        guard matching.isEmpty, environments.count == 1, let only = environments.first else {
            throw PairingError.environmentMismatch
        }
        return only
    }
}

enum PairingError: Error { case environmentMismatch, expired }
private enum AppConnectionError: Error { case authentication, missingEnvironment }

extension Notification.Name {
    static let clippySyncForegrounded = Notification.Name("clippy-sync-foregrounded")
}
