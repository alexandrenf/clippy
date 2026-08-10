import Foundation
import Testing
@testable import ClippySyncCore

@Test func relayTokenCacheIsBoundToThumbprintAndExpiry() {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    var cache = RelayTokenCache()
    cache.store(accessToken: "relay-token", expiresAt: now.addingTimeInterval(120), jkt: "jkt-a")

    #expect(cache.token(forJKT: "jkt-a", at: now) == "relay-token")
    #expect(cache.token(forJKT: "jkt-b", at: now) == nil)
    #expect(cache.token(forJKT: "jkt-a", at: now.addingTimeInterval(61)) == nil)
}

@Test func relayFlowUsesCanonicalSnakeCaseAndReusesBoundRelayToken() async throws {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let signer = StubDPoPSigner(jwkThumbprint: "device-jkt")
    let loader = RecordingLoader(responses: [
        .json(#"{"access_token":"relay-token","token_type":"DPoP","expires_in":600,"scope":"relay","cnf":{"jkt":"device-jkt"}}"#),
        .json(#"{"environments":[{"id":"workspace-1","workspace_id":"workspace-1"}]}"#),
        .json(#"{"environment_id":"workspace-1","endpoint":{"http_base_url":"https://device.clippy.saudecomalex.com","ws_base_url":"wss://device.clippy.saudecomalex.com"},"bootstrap_credential":"bootstrap","expires_at":2000000300}"#),
        .json(#"{"access_token":"environment-token","token_type":"DPoP","expires_in":300,"scope":"sync","cnf":{"jkt":"device-jkt"}}"#),
        .json(#"{"environments":[{"id":"workspace-1","workspace_id":"workspace-1"}]}"#)
    ])
    let client = try RelayClient(
        baseURL: URL(string: "https://relay.example.com")!,
        endpointHostSuffix: "clippy.saudecomalex.com",
        signer: signer,
        loader: loader,
        now: { now }
    )

    let environments = try await client.environments(workOSAccessToken: "workos-token")
    #expect(environments.first?.workspaceId == "workspace-1")
    let connection = try await client.connect(
        environmentId: environments[0].id,
        workOSAccessToken: "workos-token"
    )
    let session = try await client.bootstrap(connection)
    #expect(session.accessToken == "environment-token")
    _ = try await client.environments(workOSAccessToken: "workos-token")

    let requests = await loader.recordedRequests()
    #expect(requests.count == 5)
    #expect(requests[0].value(forHTTPHeaderField: "Authorization") == "Bearer workos-token")
    #expect(requests[1].value(forHTTPHeaderField: "Authorization") == "DPoP relay-token")
    #expect(requests[2].value(forHTTPHeaderField: "Authorization") == "DPoP relay-token")
    #expect(requests[3].value(forHTTPHeaderField: "Authorization") == "DPoP bootstrap")
    #expect(requests[4].value(forHTTPHeaderField: "Authorization") == "DPoP relay-token")
    #expect(requests.allSatisfy { $0.value(forHTTPHeaderField: "DPoP") != nil })

    let body = try #require(requests[2].httpBody)
    let object = try #require(JSONSerialization.jsonObject(with: body) as? [String: String])
    let nonce = try #require(object["client_nonce"])
    #expect(try Data(base64URLEncoded: nonce).count == 32)
}

@Test func relayRejectsTokenBoundToAnotherKey() async throws {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let loader = RecordingLoader(responses: [
        .json(#"{"access_token":"relay-token","token_type":"DPoP","expires_in":600,"cnf":{"jkt":"attacker"}}"#)
    ])
    let client = try RelayClient(
        baseURL: URL(string: "https://relay.example.com")!,
        endpointHostSuffix: "clippy.saudecomalex.com",
        signer: StubDPoPSigner(jwkThumbprint: "device-jkt"),
        loader: loader,
        now: { now }
    )

    await #expect(throws: RelayError.keyBindingMismatch) {
        try await client.environments(workOSAccessToken: "workos-token")
    }
}

@Test func websocketUpgradeContainsOnlyOneUseTicket() async throws {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let loader = RecordingLoader(responses: [
        .json(#"{"ws_ticket":"one-use-ticket","expires_in":30}"#)
    ])
    let environment = EnvironmentSession(
        environmentId: "workspace-1",
        endpoint: EnvironmentEndpoint(
            httpBaseURL: URL(string: "https://device.clippy.saudecomalex.com")!,
            wsBaseURL: URL(string: "wss://device.clippy.saudecomalex.com")!
        ),
        accessToken: "long-lived-environment-token",
        scope: "sync",
        expiresAt: now.addingTimeInterval(300)
    )
    let transport = try SyncTransport(
        environment: environment,
        signer: StubDPoPSigner(jwkThumbprint: "device-jkt"),
        session: URLSession(configuration: .ephemeral),
        loader: loader,
        now: { now }
    )
    let socket = try await transport.eventSocket()
    let request = try #require(socket.originalRequest)
    let parts = try #require(URLComponents(url: request.url!, resolvingAgainstBaseURL: false))

    #expect(parts.queryItems == [URLQueryItem(name: "wsTicket", value: "one-use-ticket")])
    #expect(request.value(forHTTPHeaderField: "Authorization") == nil)
    #expect(request.value(forHTTPHeaderField: "DPoP") == nil)
    #expect(!request.url!.absoluteString.contains("long-lived-environment-token"))

    let ticketRequest = try #require(await loader.recordedRequests().first)
    #expect(ticketRequest.value(forHTTPHeaderField: "Authorization") == "DPoP long-lived-environment-token")
    #expect(ticketRequest.value(forHTTPHeaderField: "DPoP") != nil)
}

@Test func supervisorBlocksAuthenticationUntilCredentialWake() {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    var supervisor = ConnectionSupervisor()
    supervisor.block(.authentication)
    #expect(supervisor.foregrounded(at: now) == .none)
    #expect(supervisor.setOnline(false, at: now) == .none)
    #expect(supervisor.setOnline(true, at: now) == .none)
    #expect(supervisor.credentialOrConfigurationWake(at: now) == .connect(after: 0))
}

@Test func supervisorProbesHealthyLeaseAndReplacesAfterLongBackground() {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    var supervisor = ConnectionSupervisor(longBackgroundInterval: 60)
    supervisor.connected(leaseExpiresAt: now.addingTimeInterval(600), at: now)
    _ = supervisor.backgrounded(at: now)
    #expect(supervisor.foregrounded(at: now.addingTimeInterval(10)) == .probeLease)
    _ = supervisor.backgrounded(at: now.addingTimeInterval(20))
    #expect(supervisor.foregrounded(at: now.addingTimeInterval(90)) == .replaceConnection)
}

@Test func supervisorRetriesForeverAtCapAndOfflineOwnsNoTimer() {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    var supervisor = ConnectionSupervisor()
    let expected: [TimeInterval] = [1, 2, 4, 8, 16, 16, 16]
    for delay in expected {
        #expect(supervisor.transientFailure(at: now) == .connect(after: delay))
    }
    #expect(supervisor.setOnline(false, at: now) == .none)
    #expect(supervisor.transientFailure(at: now) == .none)
}

@Test func stableConnectionResetsReconnectLadder() {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    var supervisor = ConnectionSupervisor(stableResetInterval: 30)
    #expect(supervisor.transientFailure(at: now) == .connect(after: 1))
    #expect(supervisor.transientFailure(at: now) == .connect(after: 2))
    supervisor.connected(leaseExpiresAt: now.addingTimeInterval(600), at: now)
    #expect(supervisor.transientFailure(at: now.addingTimeInterval(31)) == .connect(after: 1))
}

private struct StubDPoPSigner: DPoPProofSigning {
    let jwkThumbprint: String

    func proof(method: String, url: URL, accessToken: String?) throws -> String {
        "proof:\(method):\(url.path):\(accessToken == nil ? "none" : "bound")"
    }
}

private actor RecordingLoader: HTTPDataLoading {
    private var responses: [StubResponse]
    private var requests: [URLRequest] = []

    init(responses: [StubResponse]) { self.responses = responses }

    func data(for request: URLRequest) async throws -> (Data, URLResponse) {
        requests.append(request)
        guard !responses.isEmpty else { throw StubError.noResponse }
        let response = responses.removeFirst()
        let http = HTTPURLResponse(
            url: request.url!,
            statusCode: response.status,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"]
        )!
        return (response.data, http)
    }

    func recordedRequests() -> [URLRequest] { requests }
}

private struct StubResponse: Sendable {
    let status: Int
    let data: Data

    static func json(_ value: String, status: Int = 200) -> StubResponse {
        StubResponse(status: status, data: Data(value.utf8))
    }
}

private enum StubError: Error { case noResponse }
