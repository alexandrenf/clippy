import Foundation

public struct RuntimeConfiguration: Sendable {
    public enum Environment: String, Sendable { case staging, production }

    public let environment: Environment
    public let workOSIssuer: URL
    public let workOSClientID: String
    public let redirectURI: URL
    public let convexURL: URL

    public init(bundle: Bundle = .main) throws {
        guard let environment = Environment(rawValue: try bundle.required("SYNC_ENVIRONMENT")) else {
            throw ConfigurationError.invalidEnvironment
        }
        self.environment = environment
        workOSIssuer = try Self.httpsURL(bundle.required("WORKOS_ISSUER"))
        workOSClientID = try bundle.required("WORKOS_CLIENT_ID")
        guard let redirect = URL(string: try bundle.required("WORKOS_REDIRECT_URI")),
              redirect.scheme == "clippy-sync" else {
            throw ConfigurationError.invalidRedirect
        }
        redirectURI = redirect
        convexURL = try Self.convexURL(bundle.required("CONVEX_URL"))
    }

    /// Keychain account names are environment-scoped so a staging workspace
    /// key can never be consumed by a production build. OAuth tokens use the
    /// separate app-private OAuthTokenStore and never touch Keychain.
    public func keychainAccount(_ name: String) -> String {
        "\(environment.rawValue):\(name)"
    }

    private static func httpsURL(_ value: String) throws -> URL {
        guard let url = URL(string: value), url.scheme == "https", url.host != nil,
              url.user == nil, url.password == nil else {
            throw ConfigurationError.insecureURL
        }
        return url
    }

    private static func convexURL(_ value: String) throws -> URL {
        let url = try httpsURL(value)
        guard url.host?.hasSuffix(".convex.cloud") == true,
              url.path.isEmpty || url.path == "/",
              url.query == nil, url.fragment == nil else {
            throw ConfigurationError.invalidConvexURL
        }
        return url
    }
}

public enum ConfigurationError: Error, Equatable {
    case missing(String)
    case invalidEnvironment
    case invalidRedirect
    case insecureURL
    case invalidConvexURL
}

private extension Bundle {
    func required(_ key: String) throws -> String {
        guard let value = object(forInfoDictionaryKey: key) as? String,
              !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw ConfigurationError.missing(key)
        }
        return value
    }
}
