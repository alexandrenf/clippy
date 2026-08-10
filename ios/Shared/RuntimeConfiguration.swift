import Foundation

public struct RuntimeConfiguration: Sendable {
    public enum Environment: String, Sendable { case staging, production }

    public let environment: Environment
    public let workOSIssuer: URL
    public let workOSClientID: String
    public let redirectURI: URL
    public let relayBaseURL: URL
    public let syncEndpointHostSuffix: String

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
        relayBaseURL = try Self.httpsURL(bundle.required("RELAY_BASE_URL"))
        syncEndpointHostSuffix = try Self.hostSuffix(
            bundle.required("SYNC_ENDPOINT_HOST_SUFFIX")
        )
    }

    /// Keychain account names are environment-scoped so a staging login or
    /// workspace key can never be consumed by a production build.
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

    private static func hostSuffix(_ value: String) throws -> String {
        let normalized = value
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
            .trimmingCharacters(in: CharacterSet(charactersIn: "."))
        guard !normalized.isEmpty,
              normalized.unicodeScalars.allSatisfy({
                  CharacterSet.alphanumerics.contains($0) || $0 == "." || $0 == "-"
              }),
              normalized.contains(".") else {
            throw ConfigurationError.invalidHostSuffix
        }
        return normalized
    }
}

public enum ConfigurationError: Error, Equatable {
    case missing(String)
    case invalidEnvironment
    case invalidRedirect
    case insecureURL
    case invalidHostSuffix
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
