import Foundation
import Security

public protocol HTTPDataLoading: Sendable {
    func data(for request: URLRequest) async throws -> (Data, URLResponse)
}

public struct URLSessionDataLoader: HTTPDataLoading, @unchecked Sendable {
    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    public func data(for request: URLRequest) async throws -> (Data, URLResponse) {
        try await session.data(for: request)
    }
}

public struct RelayEnvironment: Equatable, Sendable, Decodable {
    public let id: String
    public let name: String?
    public let workspaceId: String?

    private enum CodingKeys: String, CodingKey {
        case id, name, workspaceId, workspaceIdSnake = "workspace_id"
    }

    public init(id: String, name: String? = nil, workspaceId: String? = nil) {
        self.id = id
        self.name = name
        self.workspaceId = workspaceId
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        id = try values.decode(String.self, forKey: .id)
        name = try values.decodeIfPresent(String.self, forKey: .name)
        workspaceId = try values.decodeIfPresent(String.self, forKey: .workspaceIdSnake)
            ?? values.decodeIfPresent(String.self, forKey: .workspaceId)
    }
}

public struct EnvironmentEndpoint: Equatable, Sendable {
    public let httpBaseURL: URL
    public let wsBaseURL: URL
}

public struct EnvironmentConnection: Equatable, Sendable {
    public let environmentId: String
    public let endpoint: EnvironmentEndpoint
    public let bootstrapCredential: String
    public let expiresAt: Date
}

public struct EnvironmentSession: Equatable, Sendable {
    public let environmentId: String
    public let endpoint: EnvironmentEndpoint
    public let accessToken: String
    public let scope: String?
    public let expiresAt: Date

    public func isHealthy(at now: Date, minimumValidity: TimeInterval = 30) -> Bool {
        expiresAt.timeIntervalSince(now) > minimumValidity
    }
}

public struct RelayTokenCache: Sendable {
    private var cached: CachedToken?

    public init() {}

    public mutating func token(
        forJKT jkt: String,
        at now: Date,
        minimumValidity: TimeInterval = 60
    ) -> String? {
        guard let cached,
              cached.jkt == jkt,
              cached.expiresAt.timeIntervalSince(now) > minimumValidity else {
            return nil
        }
        return cached.accessToken
    }

    public mutating func store(
        accessToken: String,
        expiresAt: Date,
        jkt: String
    ) {
        cached = CachedToken(accessToken: accessToken, expiresAt: expiresAt, jkt: jkt)
    }

    public mutating func clear() { cached = nil }

    private struct CachedToken: Sendable {
        let accessToken: String
        let expiresAt: Date
        let jkt: String
    }
}

public actor RelayClient {
    private let baseURL: URL
    private let endpointHostSuffix: String
    private let signer: any DPoPProofSigning
    private let loader: any HTTPDataLoading
    private let now: @Sendable () -> Date
    private var tokenCache = RelayTokenCache()

    public init(
        baseURL: URL,
        endpointHostSuffix: String,
        signer: any DPoPProofSigning,
        loader: any HTTPDataLoading = URLSessionDataLoader(),
        now: @escaping @Sendable () -> Date = Date.init
    ) throws {
        try Self.requireSecureBaseURL(baseURL, scheme: "https")
        guard !endpointHostSuffix.isEmpty else { throw RelayError.invalidConfiguration }
        self.baseURL = baseURL
        self.endpointHostSuffix = endpointHostSuffix.lowercased()
        self.signer = signer
        self.loader = loader
        self.now = now
    }

    public var jwkThumbprint: String { signer.jwkThumbprint }

    public func environments(workOSAccessToken: String) async throws -> [RelayEnvironment] {
        let token = try await relayToken(workOSAccessToken: workOSAccessToken)
        let request = try dpopRequest(
            method: "GET",
            url: baseURL.appending(path: "v1/environments"),
            token: token,
            authorizationScheme: "DPoP"
        )
        let data = try await send(request)
        if let response = try? JSONDecoder().decode(EnvironmentListResponse.self, from: data) {
            return try validated(response.environments)
        }
        let environments = try decode([RelayEnvironment].self, from: data)
        return try validated(environments)
    }

    public func connect(
        environmentId: String,
        workOSAccessToken: String
    ) async throws -> EnvironmentConnection {
        guard environmentId.isSafePathComponent else { throw RelayError.invalidEnvironment }
        let token = try await relayToken(workOSAccessToken: workOSAccessToken)
        let endpoint = baseURL
            .appending(path: "v1/environments")
            .appending(path: environmentId)
            .appending(path: "connect")
        var request = try dpopRequest(
            method: "POST",
            url: endpoint,
            token: token,
            authorizationScheme: "DPoP"
        )
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(
            ConnectAttempt(clientNonce: try Self.randomNonce())
        )
        let wire = try decode(ConnectResponse.self, from: await send(request))
        guard wire.environmentId == environmentId,
              !wire.bootstrapCredential.isEmpty,
              !wire.bootstrapCredential.containsNewline,
              wire.expiresAt > now() else {
            throw RelayError.invalidResponse
        }
        let httpURL = try secureEndpoint(wire.endpoint.httpBaseURL, scheme: "https")
        let wsURL = try secureEndpoint(wire.endpoint.wsBaseURL, scheme: "wss")
        return EnvironmentConnection(
            environmentId: wire.environmentId,
            endpoint: EnvironmentEndpoint(httpBaseURL: httpURL, wsBaseURL: wsURL),
            bootstrapCredential: wire.bootstrapCredential,
            expiresAt: wire.expiresAt
        )
    }

    /// Consumes the short-lived bootstrap credential once. It is deliberately
    /// never cached or persisted; only the DPoP-bound environment token is
    /// returned to the caller.
    public func bootstrap(_ connection: EnvironmentConnection) async throws -> EnvironmentSession {
        guard connection.expiresAt > now() else { throw RelayError.expired }
        let endpoint = connection.endpoint.httpBaseURL.appending(path: "v1/connect/token")
        let request = try dpopRequest(
            method: "POST",
            url: endpoint,
            token: connection.bootstrapCredential,
            authorizationScheme: "DPoP"
        )
        let response = try decode(BoundTokenResponse.self, from: await send(request))
        let token = try validate(
            response,
            expectedJKT: signer.jwkThumbprint,
            requireConfirmation: false
        )
        return EnvironmentSession(
            environmentId: connection.environmentId,
            endpoint: connection.endpoint,
            accessToken: token.accessToken,
            scope: token.scope,
            expiresAt: token.expiresAt
        )
    }

    public func clearCachedRelayToken() { tokenCache.clear() }

    private func relayToken(workOSAccessToken: String) async throws -> String {
        guard !workOSAccessToken.isEmpty, !workOSAccessToken.containsNewline else {
            throw RelayError.invalidCredential
        }
        let jkt = signer.jwkThumbprint
        if let cached = tokenCache.token(forJKT: jkt, at: now()) { return cached }

        let endpoint = baseURL.appending(path: "v1/auth/token")
        let request = try dpopRequest(
            method: "POST",
            url: endpoint,
            token: workOSAccessToken,
            authorizationScheme: "Bearer"
        )
        let response = try decode(BoundTokenResponse.self, from: await send(request))
        let validated = try validate(response, expectedJKT: jkt, requireConfirmation: true)
        tokenCache.store(
            accessToken: validated.accessToken,
            expiresAt: validated.expiresAt,
            jkt: jkt
        )
        return validated.accessToken
    }

    private func validate(
        _ response: BoundTokenResponse,
        expectedJKT: String,
        requireConfirmation: Bool
    ) throws -> ValidatedBoundToken {
        guard response.tokenType.caseInsensitiveCompare("DPoP") == .orderedSame,
              !response.accessToken.isEmpty,
              !response.accessToken.containsNewline,
              response.expiresIn > 0,
              response.expiresIn <= 86_400 else {
            throw RelayError.invalidResponse
        }
        if requireConfirmation {
            guard response.confirmation?.jkt == expectedJKT else {
                throw RelayError.keyBindingMismatch
            }
        } else if let returnedJKT = response.confirmation?.jkt,
                  returnedJKT != expectedJKT {
            throw RelayError.keyBindingMismatch
        }
        return ValidatedBoundToken(
            accessToken: response.accessToken,
            expiresAt: now().addingTimeInterval(TimeInterval(response.expiresIn)),
            scope: response.scope
        )
    }

    private func dpopRequest(
        method: String,
        url: URL,
        token: String,
        authorizationScheme: String
    ) throws -> URLRequest {
        guard !token.isEmpty, !token.containsNewline else { throw RelayError.invalidCredential }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 20
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("\(authorizationScheme) \(token)", forHTTPHeaderField: "Authorization")
        request.setValue(
            try signer.proof(method: method, url: url, accessToken: token),
            forHTTPHeaderField: "DPoP"
        )
        return request
    }

    private func send(_ request: URLRequest) async throws -> Data {
        let (data, response) = try await loader.data(for: request)
        guard data.count <= 1_048_576,
              let response = response as? HTTPURLResponse else {
            throw RelayError.invalidResponse
        }
        guard (200..<300).contains(response.statusCode) else {
            throw RelayError.http(response.statusCode)
        }
        return data
    }

    private func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        do { return try JSONDecoder().decode(type, from: data) }
        catch { throw RelayError.invalidResponse }
    }

    private func validated(_ environments: [RelayEnvironment]) throws -> [RelayEnvironment] {
        guard environments.count <= 256,
              environments.allSatisfy({ $0.id.isSafePathComponent }) else {
            throw RelayError.invalidResponse
        }
        return environments
    }

    private func secureEndpoint(_ url: URL, scheme: String) throws -> URL {
        try Self.requireSecureBaseURL(url, scheme: scheme)
        guard let host = url.host?.lowercased(),
              host == endpointHostSuffix || host.hasSuffix(".\(endpointHostSuffix)") else {
            throw RelayError.untrustedEndpoint
        }
        return url
    }

    private static func requireSecureBaseURL(_ url: URL, scheme: String) throws {
        guard url.scheme?.lowercased() == scheme,
              url.host != nil,
              url.user == nil,
              url.password == nil,
              url.query == nil,
              url.fragment == nil else {
            throw RelayError.invalidConfiguration
        }
    }

    private static func randomNonce() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw RelayError.randomFailed
        }
        return Data(bytes).base64URLEncodedString()
    }
}

private struct EnvironmentListResponse: Decodable {
    let environments: [RelayEnvironment]
}

private struct ConnectAttempt: Encodable {
    let clientNonce: String
    private enum CodingKeys: String, CodingKey { case clientNonce = "client_nonce" }
}

private struct BoundTokenResponse: Decodable {
    let accessToken: String
    let tokenType: String
    let expiresIn: Int
    let scope: String?
    let confirmation: Confirmation?

    struct Confirmation: Decodable { let jkt: String }

    private enum CodingKeys: String, CodingKey {
        case accessToken, accessTokenSnake = "access_token"
        case tokenType, tokenTypeSnake = "token_type"
        case expiresIn, expiresInSnake = "expires_in"
        case scope, cnf, confirmation
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        accessToken = try values.decodeIfPresent(String.self, forKey: .accessTokenSnake)
            ?? values.decode(String.self, forKey: .accessToken)
        tokenType = try values.decodeIfPresent(String.self, forKey: .tokenTypeSnake)
            ?? values.decode(String.self, forKey: .tokenType)
        expiresIn = try values.decodeIfPresent(Int.self, forKey: .expiresInSnake)
            ?? values.decode(Int.self, forKey: .expiresIn)
        scope = try values.decodeIfPresent(String.self, forKey: .scope)
        confirmation = try values.decodeIfPresent(Confirmation.self, forKey: .cnf)
            ?? values.decodeIfPresent(Confirmation.self, forKey: .confirmation)
    }
}

private struct ConnectResponse: Decodable {
    let environmentId: String
    let endpoint: WireEndpoint
    let bootstrapCredential: String
    let expiresAt: Date

    struct WireEndpoint: Decodable {
        let httpBaseURL: URL
        let wsBaseURL: URL

        private enum CodingKeys: String, CodingKey {
            case httpBaseURL = "httpBaseUrl", httpBaseURLSnake = "http_base_url"
            case wsBaseURL = "wsBaseUrl", wsBaseURLSnake = "ws_base_url"
        }

        init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            httpBaseURL = try values.decodeIfPresent(URL.self, forKey: .httpBaseURLSnake)
                ?? values.decode(URL.self, forKey: .httpBaseURL)
            wsBaseURL = try values.decodeIfPresent(URL.self, forKey: .wsBaseURLSnake)
                ?? values.decode(URL.self, forKey: .wsBaseURL)
        }
    }

    private enum CodingKeys: String, CodingKey {
        case environmentId, environmentIdSnake = "environment_id", endpoint
        case bootstrapCredential, bootstrapCredentialSnake = "bootstrap_credential"
        case expiresAt, expiresAtSnake = "expires_at"
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        environmentId = try values.decodeIfPresent(String.self, forKey: .environmentIdSnake)
            ?? values.decode(String.self, forKey: .environmentId)
        endpoint = try values.decode(WireEndpoint.self, forKey: .endpoint)
        bootstrapCredential = try values.decodeIfPresent(String.self, forKey: .bootstrapCredentialSnake)
            ?? values.decode(String.self, forKey: .bootstrapCredential)
        let expiry = try values.decodeIfPresent(FlexibleExpiry.self, forKey: .expiresAtSnake)
            ?? values.decode(FlexibleExpiry.self, forKey: .expiresAt)
        expiresAt = expiry.date
    }
}

private struct FlexibleExpiry: Decodable {
    let date: Date

    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if let number = try? value.decode(Double.self) {
            date = Date(timeIntervalSince1970: number > 10_000_000_000 ? number / 1_000 : number)
            return
        }
        let string = try value.decode(String.self)
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let parsed = fractional.date(from: string) ?? ISO8601DateFormatter().date(from: string) {
            date = parsed
            return
        }
        throw RelayError.invalidResponse
    }
}

private struct ValidatedBoundToken {
    let accessToken: String
    let expiresAt: Date
    let scope: String?
}

public enum RelayError: Error, Equatable, Sendable {
    case invalidConfiguration
    case invalidCredential
    case invalidEnvironment
    case invalidResponse
    case keyBindingMismatch
    case untrustedEndpoint
    case expired
    case randomFailed
    case http(Int)
}

private extension String {
    var containsNewline: Bool { contains("\n") || contains("\r") }

    var isSafePathComponent: Bool {
        !isEmpty && utf8.count <= 128 && allSatisfy {
            $0.isLetter || $0.isNumber || $0 == "-" || $0 == "_" || $0 == "."
        }
    }
}
