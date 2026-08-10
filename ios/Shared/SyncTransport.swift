import Foundation

public actor SyncTransport {
    private let environment: EnvironmentSession
    private let signer: any DPoPProofSigning
    private let session: URLSession
    private let loader: any HTTPDataLoading
    private let now: @Sendable () -> Date

    public init(
        environment: EnvironmentSession,
        signer: any DPoPProofSigning,
        session: URLSession = .shared,
        loader: (any HTTPDataLoading)? = nil,
        now: @escaping @Sendable () -> Date = Date.init
    ) throws {
        guard environment.endpoint.httpBaseURL.scheme == "https",
              environment.endpoint.wsBaseURL.scheme == "wss",
              environment.endpoint.httpBaseURL.host != nil,
              environment.endpoint.wsBaseURL.host != nil else {
            throw TransportError.insecureTransport
        }
        self.environment = environment
        self.signer = signer
        self.session = session
        self.loader = loader ?? URLSessionDataLoader(session: session)
        self.now = now
    }

    public var leaseExpiresAt: Date { environment.expiresAt }
    public var environmentId: String { environment.environmentId }

    public func enroll(request enrollment: AccountEnrollmentRequest) async throws -> AccountEnrollmentResponse {
        var request = try authenticatedRequest(path: "v1/sync/enroll", method: "POST")
        request.httpBody = try JSONEncoder().encode(enrollment)
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        return try await send(request, as: AccountEnrollmentResponse.self)
    }

    public func exchange(
        envelope: SealedEnvelope,
        deviceId: String
    ) async throws -> SealedEnvelope {
        var request = try authenticatedRequest(path: "v1/sync/exchange", method: "POST")
        request.httpBody = try JSONEncoder().encode(
            ExchangeRequest(deviceId: deviceId, envelope: envelope)
        )
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let response = try await send(request, as: ExchangeResponse.self)
        return response.envelope
    }

    public func missingChunks(hashes: [String]) async throws -> Set<String> {
        var request = try authenticatedRequest(path: "v1/sync/chunks/missing", method: "POST")
        request.httpBody = try JSONEncoder().encode(["hashes": hashes])
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let response = try await send(request, as: MissingChunksResponse.self)
        return Set(response.hashes)
    }

    public func uploadChunk(hash: String, sealedChunk: SealedEnvelope) async throws {
        guard hash.isLowercaseSHA256 else { throw TransportError.invalidHash }
        var request = try authenticatedRequest(path: "v1/sync/chunks/\(hash)", method: "PUT")
        request.httpBody = try JSONEncoder().encode(sealedChunk)
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let (_, response) = try await loader.data(for: request)
        try validate(response)
    }

    public func downloadChunk(hash: String) async throws -> SealedEnvelope {
        guard hash.isLowercaseSHA256 else { throw TransportError.invalidHash }
        let request = try authenticatedRequest(path: "v1/sync/chunks/\(hash)", method: "GET")
        return try await send(request, as: SealedEnvelope.self)
    }

    /// The one-use ticket is the only credential carried by the WebSocket
    /// upgrade. Long-lived access tokens and DPoP proofs never enter its URL or
    /// headers, so URLSession diagnostics cannot leak them with the socket.
    public func eventSocket() async throws -> URLSessionWebSocketTask {
        let ticketRequest = try authenticatedRequest(
            path: "v1/connect/websocket-ticket",
            method: "POST"
        )
        let ticket = try await send(ticketRequest, as: WebSocketTicketResponse.self)
        guard !ticket.wsTicket.isEmpty,
              ticket.wsTicket.utf8.count <= 4_096,
              !ticket.wsTicket.contains("\n"),
              !ticket.wsTicket.contains("\r"),
              ticket.expiresIn > 0,
              ticket.expiresIn <= 300 else {
            throw TransportError.invalidResponse
        }
        var parts = URLComponents(
            url: environment.endpoint.wsBaseURL.appending(path: "v1/sync/events"),
            resolvingAgainstBaseURL: false
        )
        parts?.queryItems = [URLQueryItem(name: "wsTicket", value: ticket.wsTicket)]
        guard let url = parts?.url else { throw TransportError.invalidResponse }
        var request = URLRequest(url: url)
        request.timeoutInterval = 45
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        return session.webSocketTask(with: request)
    }

    private func authenticatedRequest(path: String, method: String) throws -> URLRequest {
        let token = environment.accessToken
        guard environment.isHealthy(at: now(), minimumValidity: 5) else {
            throw TransportError.expiredLease
        }
        guard !token.isEmpty, !token.contains("\n"), !token.contains("\r") else {
            throw TransportError.invalidToken
        }
        let endpoint = environment.endpoint.httpBaseURL.appending(path: path)
        var request = URLRequest(url: endpoint)
        request.httpMethod = method
        request.setValue("DPoP \(token)", forHTTPHeaderField: "Authorization")
        request.setValue(
            try signer.proof(method: method, url: endpoint, accessToken: token),
            forHTTPHeaderField: "DPoP"
        )
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        request.timeoutInterval = 20
        return request
    }

    private func send<T: Decodable>(_ request: URLRequest, as: T.Type) async throws -> T {
        let (data, response) = try await loader.data(for: request)
        try validate(response)
        guard data.count <= 1_048_576 else { throw TransportError.invalidResponse }
        do { return try JSONDecoder().decode(T.self, from: data) }
        catch { throw TransportError.invalidResponse }
    }

    private func validate(_ response: URLResponse) throws {
        guard let response = response as? HTTPURLResponse else {
            throw TransportError.invalidResponse
        }
        guard (200..<300).contains(response.statusCode) else {
            throw TransportError.http(response.statusCode)
        }
    }
}

private struct ExchangeRequest: Encodable { let deviceId: String; let envelope: SealedEnvelope }
private struct ExchangeResponse: Decodable { let envelope: SealedEnvelope }
private struct MissingChunksResponse: Decodable { let hashes: [String] }

private struct WebSocketTicketResponse: Decodable {
    let wsTicket: String
    let expiresIn: Int

    private enum CodingKeys: String, CodingKey {
        case wsTicket, wsTicketSnake = "ws_ticket"
        case expiresIn, expiresInSnake = "expires_in"
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        wsTicket = try values.decodeIfPresent(String.self, forKey: .wsTicketSnake)
            ?? values.decode(String.self, forKey: .wsTicket)
        expiresIn = try values.decodeIfPresent(Int.self, forKey: .expiresInSnake)
            ?? values.decode(Int.self, forKey: .expiresIn)
    }
}

public enum TransportError: Error, Equatable, Sendable {
    case insecureTransport
    case invalidToken
    case invalidHash
    case invalidResponse
    case expiredLease
    case http(Int)
}

private extension String {
    var isLowercaseSHA256: Bool {
        count == 64 && allSatisfy { $0.isNumber || ("a"..."f").contains(String($0)) }
    }
}
