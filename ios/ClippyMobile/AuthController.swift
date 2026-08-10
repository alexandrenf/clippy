import AuthenticationServices
import Combine
import CryptoKit
import Foundation
import Security
import UIKit

@MainActor
final class AuthController: NSObject, ObservableObject, ASWebAuthenticationPresentationContextProviding {
    @Published private(set) var signedIn = false
    @Published private(set) var errorMessage: String?

    private let configuration: RuntimeConfiguration
    private let keychain: KeychainStore
    private let session: URLSession
    private let verifier: JWTVerifier
    private var webSession: ASWebAuthenticationSession?
    private var refreshTask: Task<ValidatedSession, Error>?

    init(
        configuration: RuntimeConfiguration,
        keychain: KeychainStore = KeychainStore(),
        session: URLSession = .shared,
        verifier: JWTVerifier? = nil
    ) {
        self.configuration = configuration
        self.keychain = keychain
        self.session = session
        guard let verifier = verifier ?? (try? JWTVerifier(
            issuer: configuration.workOSIssuer,
            audience: configuration.workOSClientID,
            session: session
        )) else {
            preconditionFailure("WorkOS JWT verifier configuration is invalid")
        }
        self.verifier = verifier
        super.init()
        Task { [weak self] in
            do { _ = try await self?.validatedStoredSession(minimumValidity: 60) }
            catch { self?.signOut() }
        }
    }

    func signIn() {
        do {
            let verifier = try Self.randomVerifier()
            let state = try Self.randomVerifier()
            let nonce = try Self.randomVerifier()
            let challenge = Data(SHA256.hash(data: Data(verifier.utf8))).base64URLEncodedString()
            var parts = URLComponents(
                url: configuration.workOSIssuer.appending(path: "oauth2/authorize"),
                resolvingAgainstBaseURL: false
            )
            parts?.queryItems = [
                URLQueryItem(name: "client_id", value: configuration.workOSClientID),
                URLQueryItem(name: "redirect_uri", value: configuration.redirectURI.absoluteString),
                URLQueryItem(name: "response_type", value: "code"),
                URLQueryItem(name: "scope", value: "openid profile email offline_access"),
                URLQueryItem(name: "code_challenge", value: challenge),
                URLQueryItem(name: "code_challenge_method", value: "S256"),
                URLQueryItem(name: "state", value: state),
                URLQueryItem(name: "nonce", value: nonce)
            ]
            guard let url = parts?.url else { throw AuthError.invalidAuthorizationURL }
            let session = ASWebAuthenticationSession(
                url: url,
                callbackURLScheme: configuration.redirectURI.scheme
            ) { [weak self] callback, error in
                guard let self, error == nil, let callback else { return }
                Task { @MainActor in
                    await self.finish(
                        callback: callback,
                        verifier: verifier,
                        expectedState: state,
                        expectedNonce: nonce
                    )
                }
            }
            session.presentationContextProvider = self
            session.prefersEphemeralWebBrowserSession = false
            webSession = session
            session.start()
        } catch {
            errorMessage = "Could not start secure sign-in."
        }
    }

    func accessToken() async throws -> String {
        do {
            return try await validatedStoredSession(minimumValidity: 60).accessToken
        } catch JWTVerificationError.expired {
            return try await refreshSession().accessToken
        } catch AuthError.refreshRequired {
            return try await refreshSession().accessToken
        } catch {
            try? deleteOAuthCredentials()
            signedIn = false
            throw error
        }
    }

    func principal() async throws -> AuthenticatedPrincipal {
        let session: ValidatedSession
        do {
            session = try await validatedStoredSession(minimumValidity: 60)
        } catch JWTVerificationError.expired {
            session = try await refreshSession()
        } catch AuthError.refreshRequired {
            session = try await refreshSession()
        } catch {
            try? deleteOAuthCredentials()
            signedIn = false
            throw error
        }
        return AuthenticatedPrincipal(
            subject: session.access.subject,
            organizationId: session.access.organizationId
        )
    }

    func signOut() {
        refreshTask?.cancel()
        refreshTask = nil
        try? deleteOAuthCredentials()
        signedIn = false
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .first { $0.isKeyWindow } ?? ASPresentationAnchor()
    }

    private func finish(
        callback: URL,
        verifier: String,
        expectedState: String,
        expectedNonce: String
    ) async {
        do {
            guard let parts = URLComponents(url: callback, resolvingAgainstBaseURL: false),
                  let queryItems = parts.queryItems,
                  let code = queryItems.first(where: { $0.name == "code" })?.value,
                  let returnedState = queryItems.first(where: { $0.name == "state" })?.value,
                  Self.secureEquals(returnedState, expectedState) else {
                throw AuthError.missingCode
            }
            let tokenSet = try await exchange(fields: [
                "client_id": configuration.workOSClientID,
                "grant_type": "authorization_code",
                "code": code,
                "redirect_uri": configuration.redirectURI.absoluteString,
                "code_verifier": verifier
            ])
            let validated = try await validate(tokenSet: tokenSet, expectedNonce: expectedNonce)
            try store(tokenSet: tokenSet, replacingRefreshToken: true)
            guard validated.access.expiresAt.timeIntervalSinceNow > 60,
                  validated.id.expiresAt.timeIntervalSinceNow > 60 else {
                throw AuthError.refreshRequired
            }
            signedIn = true
            errorMessage = nil
        } catch {
            errorMessage = "Secure sign-in did not complete."
        }
    }

    private func validatedStoredSession(minimumValidity: TimeInterval) async throws -> ValidatedSession {
        guard let accessToken = try loadString(account: account("workos-access-token")),
              let idToken = try loadString(account: account("workos-id-token")) else {
            throw AuthError.signedOut
        }
        let access = try await verifier.verify(accessToken)
        let id = try await verifier.verify(idToken, kind: .id)
        try Self.requireSamePrincipal(access, id)
        signedIn = true
        if access.expiresAt.timeIntervalSinceNow <= minimumValidity ||
            id.expiresAt.timeIntervalSinceNow <= minimumValidity {
            throw AuthError.refreshRequired
        }
        return ValidatedSession(
            accessToken: accessToken,
            idToken: idToken,
            access: access,
            id: id
        )
    }

    private func refreshSession() async throws -> ValidatedSession {
        if let refreshTask { return try await refreshTask.value }
        let task = Task { @MainActor [weak self] () throws -> ValidatedSession in
            guard let self else { throw AuthError.signedOut }
            return try await self.performRefresh()
        }
        refreshTask = task
        do {
            let session = try await task.value
            refreshTask = nil
            return session
        } catch {
            refreshTask = nil
            try? deleteOAuthCredentials()
            signedIn = false
            errorMessage = "Your session expired. Sign in again."
            throw error
        }
    }

    private func performRefresh() async throws -> ValidatedSession {
        guard let refreshToken = try loadString(account: account("workos-refresh-token")) else {
            throw AuthError.signedOut
        }
        let tokenSet = try await exchange(fields: [
            "client_id": configuration.workOSClientID,
            "grant_type": "refresh_token",
            "refresh_token": refreshToken
        ])
        let validated = try await validate(tokenSet: tokenSet, expectedNonce: nil)
        try store(tokenSet: tokenSet, replacingRefreshToken: false)
        signedIn = true
        return validated
    }

    private func exchange(fields: [String: String]) async throws -> TokenSet {
        var request = URLRequest(url: configuration.workOSIssuer.appending(path: "oauth2/token"))
        request.httpMethod = "POST"
        request.timeoutInterval = 20
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        request.httpBody = fields
            .map { "\($0.key.formEncoded)=\($0.value.formEncoded)" }
            .sorted()
            .joined(separator: "&")
            .data(using: .utf8)
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse,
              response.statusCode == 200,
              data.count <= 1_048_576 else {
            throw AuthError.tokenExchangeFailed
        }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        do { return try decoder.decode(TokenSet.self, from: data) }
        catch { throw AuthError.tokenExchangeFailed }
    }

    private func validate(tokenSet: TokenSet, expectedNonce: String?) async throws -> ValidatedSession {
        let access = try await verifier.verify(tokenSet.accessToken)
        let id = try await verifier.verify(
            tokenSet.idToken,
            kind: .id,
            expectedNonce: expectedNonce
        )
        try Self.requireSamePrincipal(access, id)
        return ValidatedSession(
            accessToken: tokenSet.accessToken,
            idToken: tokenSet.idToken,
            access: access,
            id: id
        )
    }

    private func store(tokenSet: TokenSet, replacingRefreshToken: Bool) throws {
        do {
            try keychain.save(Data(tokenSet.accessToken.utf8), account: account("workos-access-token"))
            try keychain.save(Data(tokenSet.idToken.utf8), account: account("workos-id-token"))
            if let refresh = tokenSet.refreshToken {
                try keychain.save(Data(refresh.utf8), account: account("workos-refresh-token"))
            } else if replacingRefreshToken {
                try keychain.delete(account: account("workos-refresh-token"))
            }
        } catch {
            try? deleteOAuthCredentials()
            throw error
        }
    }

    private func loadString(account: String) throws -> String? {
        guard let data = try keychain.load(account: account) else { return nil }
        guard let value = String(data: data, encoding: .utf8), !value.isEmpty else {
            throw AuthError.invalidToken
        }
        return value
    }

    private func deleteOAuthCredentials() throws {
        for name in ["workos-access-token", "workos-id-token", "workos-refresh-token"] {
            try keychain.delete(account: account(name))
        }
    }

    private func account(_ name: String) -> String { configuration.keychainAccount(name) }

    private static func requireSamePrincipal(_ access: VerifiedJWT, _ id: VerifiedJWT) throws {
        guard secureEquals(access.subject, id.subject),
              access.organizationId == id.organizationId else {
            throw AuthError.invalidToken
        }
    }

    private static func secureEquals(_ left: String, _ right: String) -> Bool {
        let left = Array(left.utf8)
        let right = Array(right.utf8)
        guard left.count == right.count else { return false }
        return zip(left, right).reduce(UInt8(0)) { $0 | ($1.0 ^ $1.1) } == 0
    }

    private static func randomVerifier() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw AuthError.randomFailed
        }
        return Data(bytes).base64URLEncodedString()
    }
}

private struct TokenSet: Decodable {
    let accessToken: String
    let idToken: String
    let refreshToken: String?
}

private struct ValidatedSession: Sendable {
    let accessToken: String
    let idToken: String
    let access: VerifiedJWT
    let id: VerifiedJWT
}

private enum AuthError: Error {
    case invalidAuthorizationURL
    case missingCode
    case tokenExchangeFailed
    case randomFailed
    case signedOut
    case invalidToken
    case refreshRequired
}

private extension String {
    var formEncoded: String {
        addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? self
    }
}
