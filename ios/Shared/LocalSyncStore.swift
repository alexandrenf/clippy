import CryptoKit
import Foundation

public struct LocalSection: Codable, Equatable, Identifiable, Sendable {
    public let id: UUID
    public var name: String
    public var sortIndex: Double
    public var isDeleted: Bool
}

public struct LocalItem: Codable, Equatable, Identifiable, Sendable {
    public let id: UUID
    public var sectionId: UUID?
    public var createdAt: UInt64?
    public var content: ContentRegister
    public var done: Bool
    public var isDeleted: Bool

    public var projectedContent: String { content.projectedValue ?? "" }

    public init(
        id: UUID,
        sectionId: UUID?,
        createdAt: UInt64?,
        content: ContentRegister,
        done: Bool,
        isDeleted: Bool
    ) {
        self.id = id
        self.sectionId = sectionId
        self.createdAt = createdAt
        self.content = content
        self.done = done
        self.isDeleted = isDeleted
    }

    private enum CodingKeys: String, CodingKey {
        case id, sectionId, createdAt, content, done, isDeleted
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        id = try values.decode(UUID.self, forKey: .id)
        sectionId = try values.decodeIfPresent(UUID.self, forKey: .sectionId)
        if let milliseconds = try? values.decode(UInt64.self, forKey: .createdAt) {
            createdAt = milliseconds
        } else if let legacy = try? values.decode(String.self, forKey: .createdAt),
                  let date = ISO8601DateFormatter().date(from: legacy) {
            createdAt = UInt64(max(0, date.timeIntervalSince1970 * 1_000))
        } else {
            createdAt = nil
        }
        content = try values.decode(ContentRegister.self, forKey: .content)
        done = try values.decode(Bool.self, forKey: .done)
        isDeleted = try values.decode(Bool.self, forKey: .isDeleted)
    }

    public func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(id, forKey: .id)
        try values.encodeIfPresent(sectionId, forKey: .sectionId)
        try values.encodeIfPresent(createdAt, forKey: .createdAt)
        try values.encode(content, forKey: .content)
        try values.encode(done, forKey: .done)
        try values.encode(isDeleted, forKey: .isDeleted)
    }
}

public struct LocalAttachment: Codable, Equatable, Identifiable, Sendable {
    public let id: UUID
    public var itemId: UUID?
    public var name: String
    public var mediaType: String
    public var size: UInt64?
    public var manifest: FileManifest?
    public var isDeleted: Bool
}

public struct LocalSyncView: Equatable, Sendable {
    public let actorId: String
    public let sections: [LocalSection]
    public let items: [LocalItem]
    public let attachments: [LocalAttachment]
    public let pendingOperationCount: Int

    public static let empty = LocalSyncView(
        actorId: "",
        sections: [],
        items: [],
        attachments: [],
        pendingOperationCount: 0
    )

    public func items(in sectionId: UUID) -> [LocalItem] {
        items.filter { $0.sectionId == sectionId }
    }

    public var inboxItems: [LocalItem] {
        items.filter { $0.sectionId == nil }
    }

    public func attachments(for itemId: UUID) -> [LocalAttachment] {
        attachments.filter { $0.itemId == itemId }
    }
}

public struct RemoteApplyResult: Equatable, Sendable {
    public let appliedOperationCount: Int
    public let acknowledgedOperationCount: Int
}

/// A small, crash-safe local replica. Immutable operations and the projected
/// records are committed together to one atomically replaced snapshot. File
/// chunks are content-addressed and are always ChaChaPoly envelopes on disk.
public actor LocalSyncStore {
    private static let snapshotSchemaVersion: UInt16 = 1
    private static let maxTextBytes = 4 * 1_024 * 1_024
    public static let maxAttachmentBytes: UInt64 = 250 * 1_024 * 1_024
    private static let maxChunkBytes = 1_048_576
    private static let maxChunkCount = 1_024
    private static let maxExactlyRepresentableJSONInteger: Double = 9_007_199_254_740_991

    private struct Snapshot: Codable, Sendable {
        var schemaVersion: UInt16
        var workspaceId: String
        var actorId: String
        var counter: UInt64
        var frontier: VersionVector
        var appliedOperationIds: Set<String>
        var pendingOperations: [SyncOperation]
        var metadataDots: [String: Dot]
        var sections: [UUID: LocalSection]
        var items: [UUID: LocalItem]
        var attachments: [UUID: LocalAttachment]
    }

    private let fileManager: FileManager
    private let workspaceDirectory: URL
    private let snapshotURL: URL
    private let chunksDirectory: URL
    private var snapshot: Snapshot

    public init(
        workspaceId: String,
        actorId: String? = nil,
        baseDirectory: URL? = nil,
        fileManager: FileManager = .default
    ) throws {
        guard !workspaceId.isEmpty, workspaceId.utf8.count <= 512 else {
            throw LocalSyncStoreError.invalidWorkspace
        }
        self.fileManager = fileManager
        let base = try baseDirectory ?? Self.defaultBaseDirectory(fileManager: fileManager)
        let workspaceDirectory = base.appending(
            path: Data(SHA256.hash(data: Data(workspaceId.utf8))).hexString,
            directoryHint: .isDirectory
        )
        self.workspaceDirectory = workspaceDirectory
        snapshotURL = workspaceDirectory.appending(path: "replica-v1.json")
        chunksDirectory = workspaceDirectory.appending(path: "chunks", directoryHint: .isDirectory)
        try Self.prepareDirectory(workspaceDirectory, fileManager: fileManager)
        try Self.prepareDirectory(chunksDirectory, fileManager: fileManager)

        if fileManager.fileExists(atPath: snapshotURL.path) {
            let data = try Data(contentsOf: snapshotURL, options: [.mappedIfSafe])
            let decoded = try JSONDecoder().decode(Snapshot.self, from: data)
            guard decoded.schemaVersion == Self.snapshotSchemaVersion,
                  decoded.workspaceId == workspaceId else {
                throw LocalSyncStoreError.incompatibleSnapshot
            }
            snapshot = decoded
        } else {
            let actorId = actorId ?? UUID().uuidString.lowercased()
            guard UUID(uuidString: actorId) != nil else {
                throw LocalSyncStoreError.invalidOperation
            }
            snapshot = Snapshot(
                schemaVersion: Self.snapshotSchemaVersion,
                workspaceId: workspaceId,
                actorId: actorId.lowercased(),
                counter: 0,
                frontier: VersionVector(),
                appliedOperationIds: [],
                pendingOperations: [],
                metadataDots: [:],
                sections: [:],
                items: [:],
                attachments: [:]
            )
            try Self.persist(snapshot, to: snapshotURL, fileManager: fileManager)
        }
    }

    public func view() -> LocalSyncView {
        LocalSyncView(
            actorId: snapshot.actorId,
            sections: snapshot.sections.values
                .filter { !$0.isDeleted }
                .sorted { ($0.sortIndex, $0.id.uuidString) < ($1.sortIndex, $1.id.uuidString) },
            items: snapshot.items.values
                .filter { !$0.isDeleted }
                .sorted { $0.id.uuidString < $1.id.uuidString },
            attachments: snapshot.attachments.values
                .filter { !$0.isDeleted }
                .sorted { $0.id.uuidString < $1.id.uuidString },
            pendingOperationCount: snapshot.pendingOperations.count
        )
    }

    public func frontier() -> VersionVector { snapshot.frontier }

    @discardableResult
    public func createSection(name: String) throws -> LocalSection {
        let normalized = try Self.validatedText(name, field: "section name", allowEmpty: false)
        return try commit { state in
            let id = UUID()
            let nextIndex = (state.sections.values.map(\.sortIndex).max() ?? -1) + 1
            try Self.recordLocal(
                mutation: .setMetadata(field: "name", value: .string(normalized)),
                entityKind: "section",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
            try Self.recordLocal(
                mutation: .setMetadata(field: "sortIndex", value: .number(nextIndex)),
                entityKind: "section",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
            guard let section = state.sections[id] else { throw LocalSyncStoreError.invalidOperation }
            return section
        }
    }

    public func renameSection(id: UUID, name: String) throws {
        let normalized = try Self.validatedText(name, field: "section name", allowEmpty: false)
        try commit { state in
            guard state.sections[id]?.isDeleted == false else {
                throw LocalSyncStoreError.missingEntity
            }
            try Self.recordLocal(
                mutation: .setMetadata(field: "name", value: .string(normalized)),
                entityKind: "section",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
        }
    }

    public func deleteSection(id: UUID) throws {
        try commit { state in
            guard state.sections[id]?.isDeleted == false else {
                throw LocalSyncStoreError.missingEntity
            }
            try Self.recordLocal(
                mutation: .delete,
                entityKind: "section",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
        }
    }

    @discardableResult
    public func createItem(sectionId: UUID, content: String) throws -> LocalItem {
        let content = try Self.validatedText(content, field: "item content", allowEmpty: true)
        return try commit { state in
            guard state.sections[sectionId]?.isDeleted == false else {
                throw LocalSyncStoreError.missingEntity
            }
            let id = UUID()
            try Self.recordLocal(
                mutation: .setMetadata(
                    field: "sectionId",
                    value: .string(sectionId.uuidString.lowercased())
                ),
                entityKind: "item",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
            try Self.recordLocal(
                mutation: .setMetadata(
                    field: "createdAt",
                    value: .number((Date().timeIntervalSince1970 * 1_000).rounded(.down))
                ),
                entityKind: "item",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
            try Self.recordLocal(
                mutation: .setContent(context: state.frontier, value: content),
                entityKind: "item",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
            guard let item = state.items[id] else { throw LocalSyncStoreError.invalidOperation }
            return item
        }
    }

    public func updateItem(id: UUID, content: String) throws {
        let content = try Self.validatedText(content, field: "item content", allowEmpty: true)
        try commit { state in
            guard state.items[id]?.isDeleted == false else {
                throw LocalSyncStoreError.missingEntity
            }
            try Self.recordLocal(
                mutation: .setContent(context: state.frontier, value: content),
                entityKind: "item",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
        }
    }

    public func resolveItemConflict(id: UUID, content: String) throws {
        let content = try Self.validatedText(content, field: "item content", allowEmpty: true)
        try commit { state in
            guard let item = state.items[id], !item.isDeleted, item.content.hasConflict else {
                throw LocalSyncStoreError.noConflict
            }
            try Self.recordLocal(
                mutation: .resolveContent(context: state.frontier, value: content),
                entityKind: "item",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
        }
    }

    public func setItemCompleted(id: UUID, done: Bool) throws {
        try commit { state in
            guard state.items[id]?.isDeleted == false else {
                throw LocalSyncStoreError.missingEntity
            }
            try Self.recordLocal(
                mutation: .setMetadata(field: "done", value: .bool(done)),
                entityKind: "item",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
        }
    }

    public func deleteItem(id: UUID) throws {
        try commit { state in
            guard state.items[id]?.isDeleted == false else {
                throw LocalSyncStoreError.missingEntity
            }
            try Self.recordLocal(
                mutation: .delete,
                entityKind: "item",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
        }
    }

    @discardableResult
    public func addAttachment(
        itemId: UUID,
        name: String,
        mediaType: String,
        data: Data,
        key: WorkspaceKey,
        chunkSize: Int = 1_048_576
    ) throws -> LocalAttachment {
        let name = try Self.validatedText(name, field: "file name", allowEmpty: false)
        let mediaType = try Self.validatedText(mediaType, field: "media type", allowEmpty: false)
        guard snapshot.items[itemId]?.isDeleted == false else {
            throw LocalSyncStoreError.missingEntity
        }
        guard UInt64(data.count) <= Self.maxAttachmentBytes,
              chunkSize > 0, chunkSize <= Self.maxChunkBytes else {
            throw LocalSyncStoreError.invalidManifest
        }
        let manifest = try FileManifest.make(data: data, chunkSize: chunkSize)
        guard Double(manifest.size) <= Self.maxExactlyRepresentableJSONInteger,
              manifest.chunks.count <= Self.maxChunkCount else {
            throw LocalSyncStoreError.invalidManifest
        }
        try writeChunks(data: data, manifest: manifest, key: key)
        return try recordAttachment(
            itemId: itemId,
            name: name,
            mediaType: mediaType,
            manifest: manifest
        )
    }

    /// Streams a file through a one-chunk buffer. Neither the plaintext file
    /// nor its encrypted representation is loaded into memory in full.
    @discardableResult
    public func addAttachment(
        itemId: UUID,
        name: String,
        mediaType: String,
        fileURL: URL,
        key: WorkspaceKey,
        chunkSize: Int = 1_048_576
    ) throws -> LocalAttachment {
        let name = try Self.validatedText(name, field: "file name", allowEmpty: false)
        let mediaType = try Self.validatedText(mediaType, field: "media type", allowEmpty: false)
        guard snapshot.items[itemId]?.isDeleted == false else {
            throw LocalSyncStoreError.missingEntity
        }
        guard fileURL.isFileURL, chunkSize > 0, chunkSize <= Self.maxChunkBytes else {
            throw LocalSyncStoreError.invalidManifest
        }
        let values = try fileURL.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
        guard values.isRegularFile == true, let fileSize = values.fileSize, fileSize >= 0,
              UInt64(fileSize) <= Self.maxAttachmentBytes,
              fileSize == 0 || (fileSize + chunkSize - 1) / chunkSize <= Self.maxChunkCount else {
            throw LocalSyncStoreError.invalidManifest
        }
        let handle = try FileHandle(forReadingFrom: fileURL)
        defer { try? handle.close() }
        var fileHasher = SHA256()
        var descriptors: [ChunkDescriptor] = []
        descriptors.reserveCapacity((fileSize + chunkSize - 1) / chunkSize)
        var totalSize: UInt64 = 0

        while true {
            var chunk = Data()
            chunk.reserveCapacity(chunkSize)
            while chunk.count < chunkSize {
                guard let part = try handle.read(upToCount: chunkSize - chunk.count),
                      !part.isEmpty else { break }
                chunk.append(part)
            }
            if chunk.isEmpty { break }
            guard descriptors.count < Self.maxChunkCount else {
                throw LocalSyncStoreError.invalidManifest
            }
            fileHasher.update(data: chunk)
            let hash = chunk.sha256Hex
            descriptors.append(ChunkDescriptor(sha256: hash, size: UInt64(chunk.count)))
            totalSize += UInt64(chunk.count)
            let envelope = try SyncCrypto.seal(
                chunk,
                key: key,
                aad: SyncCrypto.chunkAAD(workspaceId: snapshot.workspaceId, hash: hash)
            )
            try Self.persistEnvelope(
                envelope,
                to: chunkURL(hash: hash),
                fileManager: fileManager
            )
        }
        guard totalSize == UInt64(fileSize),
              Double(totalSize) <= Self.maxExactlyRepresentableJSONInteger else {
            throw LocalSyncStoreError.invalidManifest
        }
        let manifest = FileManifest(
            schemaVersion: 1,
            fileSha256: Data(fileHasher.finalize()).hexString,
            size: totalSize,
            chunkSize: UInt32(chunkSize),
            chunks: descriptors
        )
        return try recordAttachment(
            itemId: itemId,
            name: name,
            mediaType: mediaType,
            manifest: manifest
        )
    }

    private func recordAttachment(
        itemId: UUID,
        name: String,
        mediaType: String,
        manifest: FileManifest
    ) throws -> LocalAttachment {

        return try commit { state in
            let id = UUID()
            let fields: [(String, JSONValue)] = [
                ("itemId", .string(itemId.uuidString.lowercased())),
                ("name", .string(name)),
                ("mediaType", .string(mediaType)),
                ("size", .number(Double(manifest.size))),
                ("manifest", Self.manifestJSON(manifest))
            ]
            for (field, value) in fields {
                try Self.recordLocal(
                    mutation: .setMetadata(field: field, value: value),
                    entityKind: "attachment",
                    entityId: id.uuidString.lowercased(),
                    state: &state
                )
            }
            guard let attachment = state.attachments[id] else {
                throw LocalSyncStoreError.invalidOperation
            }
            return attachment
        }
    }

    public func deleteAttachment(id: UUID) throws {
        try commit { state in
            guard state.attachments[id]?.isDeleted == false else {
                throw LocalSyncStoreError.missingEntity
            }
            try Self.recordLocal(
                mutation: .delete,
                entityKind: "attachment",
                entityId: id.uuidString.lowercased(),
                state: &state
            )
        }
    }

    public func outboundPayload(limit: Int = 2_000) throws -> SyncPayload {
        guard limit > 0 else { throw LocalSyncStoreError.invalidLimit }
        let operations = Array(snapshot.pendingOperations.prefix(limit))
        var advertisedCounters = snapshot.frontier.counters
        let omitted = snapshot.pendingOperations.dropFirst(operations.count)
        for (actor, actorOperations) in Dictionary(grouping: omitted, by: { $0.dot.actorId }) {
            guard let firstOmitted = actorOperations.map(\.dot.counter).min() else { continue }
            let highestIncluded = operations
                .lazy
                .filter { $0.dot.actorId == actor }
                .map(\.dot.counter)
                .max()
            // Never acknowledge a dot that is still queued behind this page.
            // Earlier counters remain safe because causal ordering requires
            // them to be either included here or acknowledged previously.
            let safeCounter = highestIncluded ?? (firstOmitted == 0 ? 0 : firstOmitted - 1)
            advertisedCounters[actor] = min(
                advertisedCounters[actor, default: 0],
                safeCounter
            )
        }
        return SyncPayload(
            workspaceId: snapshot.workspaceId,
            frontier: VersionVector(advertisedCounters),
            operations: operations
        )
    }

    @discardableResult
    public func applyRemotePayload(_ payload: SyncPayload) throws -> RemoteApplyResult {
        guard payload.schemaVersion == 1, payload.workspaceId == snapshot.workspaceId else {
            throw LocalSyncStoreError.incompatiblePayload
        }
        guard payload.operations.count <= 2_000 else { throw LocalSyncStoreError.invalidLimit }
        for operation in payload.operations {
            try Self.validate(operation: operation, workspaceId: snapshot.workspaceId)
        }
        try Self.validateNoCausalGaps(payload.operations, state: snapshot)

        return try commit { state in
            let pendingBefore = state.pendingOperations.count
            var applied = 0
            for operation in payload.operations {
                if try Self.apply(operation: operation, state: &state) { applied += 1 }
            }
            state.pendingOperations.removeAll { payload.frontier.observes($0.dot) }
            // The peer frontier acknowledges our pending dots, but only dots
            // whose operations were actually delivered enter our causal
            // frontier. This remains correct if a response is paginated or a
            // peer frontier contains operations the peer did not send yet.
            return RemoteApplyResult(
                appliedOperationCount: applied,
                acknowledgedOperationCount: pendingBefore - state.pendingOperations.count
            )
        }
    }

    public func pendingChunkHashes() -> [String] {
        let attachmentIds = Set(
            snapshot.pendingOperations
                .filter { $0.entityKind == "attachment" }
                .compactMap { Self.entityUUID($0.entityId) }
        )
        return attachmentIds
            .compactMap { id -> FileManifest? in
                guard let attachment = snapshot.attachments[id], !attachment.isDeleted else { return nil }
                return attachment.manifest
            }
            .flatMap(\.chunks)
            .map(\.sha256)
            .uniqued()
            .sorted()
    }

    /// All locally materialized attachment chunks, including chunks whose
    /// manifest operation was already acknowledged. This lets a resumed upload
    /// finish without relying on a pending metadata operation as an index.
    public func availableChunkHashes() -> [String] {
        snapshot.attachments.values
            .filter { !$0.isDeleted }
            .compactMap(\.manifest)
            .flatMap(\.chunks)
            .map(\.sha256)
            .filter { fileManager.fileExists(atPath: chunkURL(hash: $0).path) }
            .uniqued()
            .sorted()
    }

    public func missingChunkHashes() -> [String] {
        snapshot.attachments.values
            .filter { !$0.isDeleted }
            .compactMap(\.manifest)
            .flatMap(\.chunks)
            .map(\.sha256)
            .filter { !fileManager.fileExists(atPath: chunkURL(hash: $0).path) }
            .uniqued()
            .sorted()
    }

    public func sealedChunk(hash: String) throws -> SealedEnvelope {
        guard Self.isSHA256(hash) else { throw LocalSyncStoreError.invalidChunkHash }
        let data = try Data(contentsOf: chunkURL(hash: hash), options: [.mappedIfSafe])
        return try JSONDecoder().decode(SealedEnvelope.self, from: data)
    }

    public func saveRemoteChunk(
        hash: String,
        envelope: SealedEnvelope,
        key: WorkspaceKey
    ) throws {
        guard Self.isSHA256(hash) else { throw LocalSyncStoreError.invalidChunkHash }
        let plaintext = try SyncCrypto.open(
            envelope,
            key: key,
            aad: SyncCrypto.chunkAAD(workspaceId: snapshot.workspaceId, hash: hash)
        )
        guard plaintext.sha256Hex == hash else { throw LocalSyncStoreError.chunkHashMismatch }
        try Self.persistEnvelope(envelope, to: chunkURL(hash: hash), fileManager: fileManager)
    }

    public func reconstructAttachment(id: UUID, key: WorkspaceKey) throws -> Data {
        guard let attachment = snapshot.attachments[id], !attachment.isDeleted,
              let manifest = attachment.manifest else {
            throw LocalSyncStoreError.missingEntity
        }
        var result = Data()
        if manifest.size <= UInt64(Int.max) { result.reserveCapacity(Int(manifest.size)) }
        for descriptor in manifest.chunks {
            let envelope = try sealedChunk(hash: descriptor.sha256)
            let plaintext = try SyncCrypto.open(
                envelope,
                key: key,
                aad: SyncCrypto.chunkAAD(
                    workspaceId: snapshot.workspaceId,
                    hash: descriptor.sha256
                )
            )
            guard plaintext.count == descriptor.size,
                  plaintext.sha256Hex == descriptor.sha256 else {
                throw LocalSyncStoreError.chunkHashMismatch
            }
            result.append(plaintext)
        }
        guard manifest.verify(reconstructed: result) else {
            throw LocalSyncStoreError.fileHashMismatch
        }
        return result
    }

    private func commit<T>(_ mutation: (inout Snapshot) throws -> T) throws -> T {
        let previous = snapshot
        do {
            let result = try mutation(&snapshot)
            try Self.persist(snapshot, to: snapshotURL, fileManager: fileManager)
            return result
        } catch {
            snapshot = previous
            throw error
        }
    }

    private func writeChunks(data: Data, manifest: FileManifest, key: WorkspaceKey) throws {
        var offset = 0
        for descriptor in manifest.chunks {
            let count = Int(descriptor.size)
            guard count >= 0, offset <= data.count, count <= data.count - offset else {
                throw LocalSyncStoreError.invalidManifest
            }
            let chunk = Data(data[offset..<(offset + count)])
            guard chunk.sha256Hex == descriptor.sha256 else {
                throw LocalSyncStoreError.chunkHashMismatch
            }
            let envelope = try SyncCrypto.seal(
                chunk,
                key: key,
                aad: SyncCrypto.chunkAAD(
                    workspaceId: snapshot.workspaceId,
                    hash: descriptor.sha256
                )
            )
            try Self.persistEnvelope(
                envelope,
                to: chunkURL(hash: descriptor.sha256),
                fileManager: fileManager
            )
            offset += count
        }
        guard offset == data.count else { throw LocalSyncStoreError.invalidManifest }
    }

    private func chunkURL(hash: String) -> URL {
        chunksDirectory.appending(path: "\(hash).chunk")
    }

    private static func recordLocal(
        mutation: SyncOperation.Mutation,
        entityKind: String,
        entityId: String,
        state: inout Snapshot
    ) throws {
        guard state.counter < UInt64.max else { throw LocalSyncStoreError.counterExhausted }
        state.counter += 1
        let operation = SyncOperation(
            schemaVersion: 1,
            workspaceId: state.workspaceId,
            entityKind: entityKind,
            entityId: entityId,
            dot: Dot(actorId: state.actorId, counter: state.counter),
            mutation: mutation
        )
        _ = try apply(operation: operation, state: &state)
        state.pendingOperations.append(operation)
    }

    @discardableResult
    private static func apply(operation: SyncOperation, state: inout Snapshot) throws -> Bool {
        try validate(operation: operation, workspaceId: state.workspaceId)
        let operationId = operationId(operation.dot)
        guard !state.appliedOperationIds.contains(operationId) else { return false }
        guard let id = entityUUID(operation.entityId) else {
            throw LocalSyncStoreError.invalidEntityId
        }

        switch operation.mutation {
        case let .setMetadata(field, value):
            try applyMetadata(
                field: field,
                value: value,
                entityKind: operation.entityKind,
                id: id,
                dot: operation.dot,
                state: &state
            )
        case let .setContent(context, value), let .resolveContent(context, value):
            guard operation.entityKind == "item" else {
                throw LocalSyncStoreError.invalidOperation
            }
            _ = try validatedText(value, field: "item content", allowEmpty: true)
            var item = state.items[id] ?? LocalItem(
                id: id,
                sectionId: nil,
                createdAt: nil,
                content: ContentRegister(),
                done: false,
                isDeleted: false
            )
            item.content.apply(ContentVersion(dot: operation.dot, context: context, value: value))
            state.items[id] = item
        case .delete:
            try applyDelete(entityKind: operation.entityKind, id: id, dot: operation.dot, state: &state)
        }

        state.appliedOperationIds.insert(operationId)
        advanceFrontier(for: operation.dot.actorId, state: &state)
        return true
    }

    private static func validateNoCausalGaps(
        _ operations: [SyncOperation],
        state: Snapshot
    ) throws {
        let byActor = Dictionary(grouping: operations, by: { $0.dot.actorId })
        for (actor, actorOperations) in byActor {
            let counters = Set(actorOperations.compactMap { operation -> UInt64? in
                state.appliedOperationIds.contains(operationId(operation.dot))
                    ? nil
                    : operation.dot.counter
            }).sorted()
            let current = state.frontier.counters[actor, default: 0]
            if current == UInt64.max {
                if !counters.isEmpty { throw LocalSyncStoreError.counterExhausted }
                continue
            }
            var expected = current + 1
            for counter in counters {
                guard counter == expected else { throw LocalSyncStoreError.causalGap }
                if expected == UInt64.max { break }
                expected += 1
            }
        }
    }

    private static func advanceFrontier(for actor: String, state: inout Snapshot) {
        var counter = state.frontier.counters[actor, default: 0]
        while counter < UInt64.max {
            let next = counter + 1
            guard state.appliedOperationIds.contains(
                operationId(Dot(actorId: actor, counter: next))
            ) else { break }
            state.frontier.observe(Dot(actorId: actor, counter: next))
            counter = next
        }
    }

    private static func operationId(_ dot: Dot) -> String {
        "\(dot.actorId.utf8.count):\(dot.actorId):\(dot.counter)"
    }

    private static func applyMetadata(
        field: String,
        value: JSONValue,
        entityKind: String,
        id: UUID,
        dot: Dot,
        state: inout Snapshot
    ) throws {
        let clockKey = "\(entityKind):\(id.uuidString.lowercased()):\(field)"
        if let existing = state.metadataDots[clockKey], existing >= dot { return }

        switch (entityKind, field, value) {
        case let ("section", "name", .string(name)):
            var section = state.sections[id] ?? LocalSection(
                id: id,
                name: "Untitled",
                sortIndex: 0,
                isDeleted: false
            )
            section.name = try validatedText(name, field: "section name", allowEmpty: false)
            state.sections[id] = section
        case let ("section", "sortIndex", .number(index)) where index.isFinite:
            var section = state.sections[id] ?? LocalSection(
                id: id,
                name: "Untitled",
                sortIndex: 0,
                isDeleted: false
            )
            section.sortIndex = index
            state.sections[id] = section
        case let ("item", "sectionId", .string(sectionId)):
            guard let sectionId = entityUUID(sectionId) else {
                throw LocalSyncStoreError.invalidEntityId
            }
            var item = state.items[id] ?? LocalItem(
                id: id,
                sectionId: nil,
                createdAt: nil,
                content: ContentRegister(),
                done: false,
                isDeleted: false
            )
            item.sectionId = sectionId
            state.items[id] = item
        case ("item", "sectionId", .null):
            var item = state.items[id] ?? LocalItem(
                id: id,
                sectionId: nil,
                createdAt: nil,
                content: ContentRegister(),
                done: false,
                isDeleted: false
            )
            item.sectionId = nil
            state.items[id] = item
        case let ("item", "createdAt", .number(createdAt))
            where createdAt >= 0 &&
                createdAt <= maxExactlyRepresentableJSONInteger &&
                createdAt.rounded() == createdAt:
            var item = state.items[id] ?? LocalItem(
                id: id,
                sectionId: nil,
                createdAt: nil,
                content: ContentRegister(),
                done: false,
                isDeleted: false
            )
            item.createdAt = UInt64(createdAt)
            state.items[id] = item
        case let ("item", "done", .bool(done)):
            var item = state.items[id] ?? LocalItem(
                id: id,
                sectionId: nil,
                createdAt: nil,
                content: ContentRegister(),
                done: false,
                isDeleted: false
            )
            item.done = done
            state.items[id] = item
        case let ("attachment", "itemId", .string(itemId)):
            guard let itemId = entityUUID(itemId) else {
                throw LocalSyncStoreError.invalidEntityId
            }
            var attachment = state.attachments[id] ?? placeholderAttachment(id: id)
            attachment.itemId = itemId
            state.attachments[id] = attachment
        case let ("attachment", "name", .string(name)):
            var attachment = state.attachments[id] ?? placeholderAttachment(id: id)
            attachment.name = try validatedText(name, field: "file name", allowEmpty: false)
            state.attachments[id] = attachment
        case let ("attachment", "mediaType", .string(mediaType)):
            var attachment = state.attachments[id] ?? placeholderAttachment(id: id)
            attachment.mediaType = try validatedText(mediaType, field: "media type", allowEmpty: false)
            state.attachments[id] = attachment
        case let ("attachment", "size", .number(size))
            where size >= 0 && size <= Double(maxAttachmentBytes) && size.rounded() == size:
            var attachment = state.attachments[id] ?? placeholderAttachment(id: id)
            guard attachment.manifest == nil || attachment.manifest?.size == UInt64(size) else {
                throw LocalSyncStoreError.invalidManifest
            }
            attachment.size = UInt64(size)
            state.attachments[id] = attachment
        case let ("attachment", "manifest", value):
            var attachment = state.attachments[id] ?? placeholderAttachment(id: id)
            let parsed = try manifest(from: value)
            guard attachment.size == nil || attachment.size == parsed.size else {
                throw LocalSyncStoreError.invalidManifest
            }
            attachment.manifest = parsed
            state.attachments[id] = attachment
        default:
            throw LocalSyncStoreError.unsupportedField
        }
        state.metadataDots[clockKey] = dot
    }

    private static func applyDelete(
        entityKind: String,
        id: UUID,
        dot: Dot,
        state: inout Snapshot
    ) throws {
        let clockKey = "\(entityKind):\(id.uuidString.lowercased()):__deleted"
        if let existing = state.metadataDots[clockKey], existing >= dot { return }
        switch entityKind {
        case "section":
            var record = state.sections[id] ?? LocalSection(
                id: id,
                name: "Untitled",
                sortIndex: 0,
                isDeleted: false
            )
            record.isDeleted = true
            state.sections[id] = record
        case "item":
            var record = state.items[id] ?? LocalItem(
                id: id,
                sectionId: nil,
                createdAt: nil,
                content: ContentRegister(),
                done: false,
                isDeleted: false
            )
            record.isDeleted = true
            state.items[id] = record
        case "attachment":
            var record = state.attachments[id] ?? placeholderAttachment(id: id)
            record.isDeleted = true
            state.attachments[id] = record
        default:
            throw LocalSyncStoreError.unsupportedEntity
        }
        state.metadataDots[clockKey] = dot
    }

    private static func validate(operation: SyncOperation, workspaceId: String) throws {
        guard operation.schemaVersion == 1, operation.workspaceId == workspaceId else {
            throw LocalSyncStoreError.incompatiblePayload
        }
        guard ["section", "item", "attachment"].contains(operation.entityKind) else {
            throw LocalSyncStoreError.unsupportedEntity
        }
        guard entityUUID(operation.entityId) != nil,
              !operation.dot.actorId.isEmpty,
              operation.dot.actorId.utf8.count <= 512,
              operation.dot.counter > 0 else {
            throw LocalSyncStoreError.invalidOperation
        }
    }

    /// Desktop rows predate the sync protocol and use SQLite's compact
    /// 32-hex UUID representation. New mobile operations use canonical UUID
    /// strings. Accept both spellings at the compatibility boundary and use a
    /// single UUID value internally so the same entity cannot fork by format.
    private static func entityUUID(_ value: String) -> UUID? {
        if let canonical = UUID(uuidString: value) { return canonical }
        guard value.count == 32,
              value.utf8.allSatisfy({ byte in
                  (48...57).contains(byte) || (97...102).contains(byte) || (65...70).contains(byte)
              }) else { return nil }
        let characters = Array(value)
        let hyphenated = [
            String(characters[0..<8]),
            String(characters[8..<12]),
            String(characters[12..<16]),
            String(characters[16..<20]),
            String(characters[20..<32]),
        ].joined(separator: "-")
        return UUID(uuidString: hyphenated)
    }

    private static func placeholderAttachment(id: UUID) -> LocalAttachment {
        LocalAttachment(
            id: id,
            itemId: nil,
            name: "Attachment",
            mediaType: "application/octet-stream",
            size: nil,
            manifest: nil,
            isDeleted: false
        )
    }

    private static func manifestJSON(_ manifest: FileManifest) -> JSONValue {
        .object([
            "schemaVersion": .number(Double(manifest.schemaVersion)),
            "fileSha256": .string(manifest.fileSha256),
            "size": .number(Double(manifest.size)),
            "chunkSize": .number(Double(manifest.chunkSize)),
            "chunks": .array(manifest.chunks.map { descriptor in
                .object([
                    "sha256": .string(descriptor.sha256),
                    "size": .number(Double(descriptor.size))
                ])
            })
        ])
    }

    private static func manifest(from value: JSONValue) throws -> FileManifest {
        guard case let .object(object) = value,
              case let .number(schemaVersion) = object["schemaVersion"],
              schemaVersion == 1,
              case let .string(fileHash) = object["fileSha256"],
              case let .number(size) = object["size"],
              case let .number(chunkSize) = object["chunkSize"],
              case let .array(chunkValues) = object["chunks"],
              chunkValues.count <= maxChunkCount,
              size >= 0, size <= Double(maxAttachmentBytes), size.rounded() == size,
              chunkSize > 0, chunkSize <= Double(maxChunkBytes), chunkSize.rounded() == chunkSize,
              isSHA256(fileHash) else {
            throw LocalSyncStoreError.invalidManifest
        }
        let totalSize = UInt64(size)
        let declaredChunkSize = UInt64(chunkSize)
        let expectedChunkCount = totalSize == 0
            ? 0
            : Int((totalSize - 1) / declaredChunkSize + 1)
        guard chunkValues.count == expectedChunkCount else {
            throw LocalSyncStoreError.invalidManifest
        }
        let chunks: [ChunkDescriptor] = try chunkValues.map { value in
            guard case let .object(chunk) = value,
                  case let .string(hash) = chunk["sha256"],
                  case let .number(count) = chunk["size"],
                  count > 0, count <= chunkSize, count.rounded() == count,
                  isSHA256(hash) else {
                throw LocalSyncStoreError.invalidManifest
            }
            return ChunkDescriptor(sha256: hash, size: UInt64(count))
        }
        for (index, descriptor) in chunks.enumerated() {
            let expectedSize = index == chunks.count - 1
                ? totalSize - declaredChunkSize * UInt64(index)
                : declaredChunkSize
            guard descriptor.size == expectedSize else {
                throw LocalSyncStoreError.invalidManifest
            }
        }
        guard chunks.reduce(UInt64(0), { partial, descriptor in
            partial.addingReportingOverflow(descriptor.size).overflow ? UInt64.max : partial + descriptor.size
        }) == totalSize else {
            throw LocalSyncStoreError.invalidManifest
        }
        return FileManifest(
            schemaVersion: UInt8(schemaVersion),
            fileSha256: fileHash,
            size: UInt64(size),
            chunkSize: UInt32(chunkSize),
            chunks: chunks
        )
    }

    private static func validatedText(
        _ value: String,
        field: String,
        allowEmpty: Bool
    ) throws -> String {
        let trimmed = field == "item content" ? value : value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard (allowEmpty || !trimmed.isEmpty), trimmed.utf8.count <= maxTextBytes else {
            throw LocalSyncStoreError.invalidText(field)
        }
        return trimmed
    }

    private static func defaultBaseDirectory(fileManager: FileManager) throws -> URL {
        guard let support = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else {
            throw LocalSyncStoreError.storageUnavailable
        }
        return support.appending(path: "ClippySync", directoryHint: .isDirectory)
    }

    private static func prepareDirectory(_ url: URL, fileManager: FileManager) throws {
        try fileManager.createDirectory(
            at: url,
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutableURL = url
        try? mutableURL.setResourceValues(values)
    }

    private static func persist(
        _ snapshot: Snapshot,
        to url: URL,
        fileManager: FileManager
    ) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let data = try encoder.encode(snapshot)
        try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        try fileManager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
    }

    private static func persistEnvelope(
        _ envelope: SealedEnvelope,
        to url: URL,
        fileManager: FileManager
    ) throws {
        let data = try JSONEncoder().encode(envelope)
        try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        try fileManager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
    }

    private static func isSHA256(_ value: String) -> Bool {
        value.count == 64 && value.allSatisfy { $0.isNumber || ("a"..."f").contains(String($0)) }
    }
}

public enum LocalSyncStoreError: Error, Equatable, Sendable {
    case storageUnavailable
    case invalidWorkspace
    case incompatibleSnapshot
    case incompatiblePayload
    case unsupportedEntity
    case unsupportedField
    case invalidEntityId
    case invalidOperation
    case causalGap
    case invalidText(String)
    case missingEntity
    case noConflict
    case counterExhausted
    case invalidLimit
    case invalidManifest
    case invalidChunkHash
    case chunkHashMismatch
    case fileHashMismatch
}

private extension Data {
    var sha256Hex: String {
        SHA256.hash(data: self).map { String(format: "%02x", $0) }.joined()
    }

    var hexString: String { map { String(format: "%02x", $0) }.joined() }
}

private extension Collection where Element: Hashable {
    func uniqued() -> [Element] {
        var seen: Set<Element> = []
        return filter { seen.insert($0).inserted }
    }
}
