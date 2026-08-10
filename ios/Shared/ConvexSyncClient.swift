import Combine
@preconcurrency import ConvexMobile
import Foundation

public struct CloudActorCounter: Equatable, Decodable {
    public let actorId: String
    @ConvexFloat public var latestCounter: Double
}

public struct CloudBatch: Sendable {
    public let actorId: String
    public let firstCounter: UInt64
    public let lastCounter: UInt64
    public let envelope: SealedEnvelope
}

public struct CloudUpload: Sendable, Decodable {
    public let hash: String
    public let exists: Bool
    public let url: String?
}

public struct CloudDownload: Sendable, Decodable {
    public let hash: String
    public let url: String
}

public struct PendingCloudEnrollment: Decodable {
    public let enrollmentId: String
    public let actorId: String
    public let deviceName: String
    public let phonePublicKey: String
    @ConvexFloat public var expiresAt: Double
}

public struct EnrollmentRequestResult: Sendable, Decodable {
    public let state: String
    public let workspaceId: String?
}

public struct EnrollmentStatus: Sendable {
    public let state: String
    public let workspaceId: String?
    public let response: AccountEnrollmentResponse?
}

private final class WorkOSTokenProvider: AuthProvider, @unchecked Sendable {
    typealias T = String
    private let token: @Sendable () async throws -> String

    init(token: @escaping @Sendable () async throws -> String) {
        self.token = token
    }

    func login(onIdToken: @Sendable @escaping (String?) -> Void) async throws -> String {
        try await load(onIdToken: onIdToken)
    }

    func loginFromCache(onIdToken: @Sendable @escaping (String?) -> Void) async throws -> String {
        try await load(onIdToken: onIdToken)
    }

    func logout() async throws {}
    func extractIdToken(from authResult: String) -> String { authResult }

    private func load(onIdToken: @Sendable @escaping (String?) -> Void) async throws -> String {
        let value = try await token()
        onIdToken(value)
        return value
    }
}

@MainActor
public final class ConvexSyncClient {
    private let client: ConvexClientWithAuth<String>
    private let session: URLSession

    public init(
        deploymentURL: URL,
        session: URLSession = .shared,
        token: @escaping @Sendable () async throws -> String
    ) {
        client = ConvexClientWithAuth(
            deploymentUrl: deploymentURL.absoluteString,
            authProvider: WorkOSTokenProvider(token: token)
        )
        self.session = session
    }

    public func authenticate() async throws {
        _ = try await client.loginFromCache().get()
    }

    public func signOut() async {
        await client.logout()
    }

    public func changes(
        workspaceId: String,
        actorId: String
    ) -> AnyPublisher<[CloudActorCounter], ClientError> {
        client.subscribe(
            to: "sync:changes",
            with: ["workspaceId": workspaceId, "actorId": actorId],
            yielding: [CloudActorCounter].self
        )
    }

    public func push(workspaceId: String, batch: CloudBatch) async throws -> UInt64 {
        let response: PushWire = try await client.mutation(
            "sync:push",
            with: [
                "workspaceId": workspaceId,
                "actorId": batch.actorId,
                "firstCounter": Double(batch.firstCounter),
                "lastCounter": Double(batch.lastCounter),
                "envelope": envelopeArgs(batch.envelope),
            ]
        )
        return try integer(response.acceptedThrough)
    }

    public func pull(
        workspaceId: String,
        actorId: String,
        frontier: VersionVector
    ) async throws -> [CloudBatch] {
        let frontierArgs: [ConvexEncodable?] = frontier.counters
            .sorted { $0.key < $1.key }
            .map { entry in
                ["actorId": entry.key, "counter": Double(entry.value)] as [String: ConvexEncodable?]
            }
        let response: [CloudBatchWire] = try await queryOnce(
            "sync:pull",
            with: [
                "workspaceId": workspaceId,
                "actorId": actorId,
                "frontier": frontierArgs,
            ]
        )
        return try response.map { wire in
            CloudBatch(
                actorId: wire.actorId,
                firstCounter: try integer(wire.firstCounter),
                lastCounter: try integer(wire.lastCounter),
                envelope: wire.envelope.value
            )
        }
    }

    public func prepareUploads(workspaceId: String, hashes: [String]) async throws -> [CloudUpload] {
        let result: [CloudUpload] = try await client.action(
            "storage:prepareUploads",
            with: ["workspaceId": workspaceId, "hashes": hashes.map { $0 as ConvexEncodable? }]
        )
        return result
    }

    public func downloadURLs(workspaceId: String, hashes: [String]) async throws -> [CloudDownload] {
        let result: [CloudDownload] = try await client.action(
            "storage:downloadUrls",
            with: ["workspaceId": workspaceId, "hashes": hashes.map { $0 as ConvexEncodable? }]
        )
        return result
    }

    public func upload(_ envelope: SealedEnvelope, to url: URL) async throws {
        var request = URLRequest(url: url)
        request.httpMethod = "PUT"
        request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(envelope)
        let (_, response) = try await session.data(for: request)
        try validate(response)
    }

    public func download(from url: URL) async throws -> SealedEnvelope {
        let (data, response) = try await session.data(from: url)
        try validate(response)
        guard data.count <= 2_097_152 else { throw ConvexSyncError.invalidResponse }
        return try JSONDecoder().decode(SealedEnvelope.self, from: data)
    }

    public func requestEnrollment(
        enrollmentId: String,
        actorId: String,
        deviceName: String,
        phonePublicKey: String
    ) async throws -> EnrollmentRequestResult {
        try await client.mutation(
            "sync:requestEnrollment",
            with: [
                "enrollmentId": enrollmentId,
                "actorId": actorId,
                "deviceName": deviceName,
                "phonePublicKey": phonePublicKey,
            ]
        )
    }

    public func enrollmentStatus(enrollmentId: String, actorId: String) async throws -> EnrollmentStatus? {
        let wire: EnrollmentStatusWire? = try await queryOnce(
            "sync:enrollmentStatus",
            with: ["enrollmentId": enrollmentId, "actorId": actorId]
        )
        guard let wire else { return nil }
        let response: AccountEnrollmentResponse?
        if let offer = wire.offer, let grant = wire.grant {
            response = AccountEnrollmentResponse(offer: offer.value, grant: grant.value)
        } else {
            response = nil
        }
        return EnrollmentStatus(state: wire.state, workspaceId: wire.workspaceId, response: response)
    }

    public func acceptEnrollment(enrollmentId: String, actorId: String) async throws -> String {
        let response: AcceptWire = try await client.mutation(
            "sync:acceptEnrollment",
            with: ["enrollmentId": enrollmentId, "actorId": actorId]
        )
        return response.workspaceId
    }

    private func queryOnce<T: Decodable>(
        _ name: String,
        with args: [String: ConvexEncodable?]
    ) async throws -> T {
        let values = client.subscribe(to: name, with: args, yielding: T.self).values
        for try await value in values { return value }
        throw ConvexSyncError.invalidResponse
    }

    private func validate(_ response: URLResponse) throws {
        guard let response = response as? HTTPURLResponse,
              (200..<300).contains(response.statusCode) else {
            throw ConvexSyncError.storage
        }
    }
}

private struct PushWire: Decodable {
    @ConvexFloat var acceptedThrough: Double
}

private struct CloudBatchWire: Decodable {
    let actorId: String
    @ConvexFloat var firstCounter: Double
    @ConvexFloat var lastCounter: Double
    let envelope: CloudEnvelope
}

private struct CloudEnvelope: Decodable {
    @ConvexFloat var version: Double
    let nonce: String
    let ciphertext: String

    var value: SealedEnvelope {
        SealedEnvelope(version: UInt8(version), nonce: nonce, ciphertext: ciphertext)
    }
}

private struct CloudOffer: Decodable {
    @ConvexFloat var version: Double
    let workspaceId: String
    let syncUrl: String
    let workosIssuer: String
    let workosAudience: String
    let macPublicKey: String
    let oneTimeToken: String
    @ConvexFloat var expiresAtMs: Double

    var value: PairingOffer {
        PairingOffer(
            version: UInt8(version),
            workspaceId: workspaceId,
            syncUrl: syncUrl,
            workosIssuer: workosIssuer,
            workosAudience: workosAudience,
            macPublicKey: macPublicKey,
            oneTimeToken: oneTimeToken,
            expiresAtMs: UInt64(expiresAtMs)
        )
    }
}

private struct CloudGrant: Decodable {
    let macPublicKey: String
    let phonePublicKey: String
    let sealedWorkspace: CloudEnvelope

    var value: PairingGrant {
        PairingGrant(
            macPublicKey: macPublicKey,
            phonePublicKey: phonePublicKey,
            sealedWorkspace: sealedWorkspace.value
        )
    }
}

private struct EnrollmentStatusWire: Decodable {
    let state: String
    let workspaceId: String?
    let offer: CloudOffer?
    let grant: CloudGrant?
}

private struct AcceptWire: Decodable { let workspaceId: String }

private func envelopeArgs(_ envelope: SealedEnvelope) -> [String: ConvexEncodable?] {
    [
        "version": Double(envelope.version),
        "nonce": envelope.nonce,
        "ciphertext": envelope.ciphertext,
    ]
}

private func integer(_ value: Double) throws -> UInt64 {
    guard value >= 0, value <= 9_007_199_254_740_991, value.rounded() == value else {
        throw ConvexSyncError.invalidResponse
    }
    return UInt64(value)
}

public enum ConvexSyncError: Error, Sendable {
    case invalidResponse
    case storage
}
