import Foundation

/// Deterministic connection lifecycle shared by the app and tests. It owns no
/// timers or sockets: callers execute the returned command, which makes the
/// offline state incapable of accidentally scheduling network work.
public struct ConnectionSupervisor: Sendable {
    public enum BlockReason: Equatable, Sendable {
        case authentication
        case configuration
    }

    public enum State: Equatable, Sendable {
        case offline
        case disconnected
        case connected(leaseExpiresAt: Date, establishedAt: Date)
        case blocked(BlockReason)
    }

    public enum Command: Equatable, Sendable {
        case none
        case connect(after: TimeInterval)
        case probeLease
        case replaceConnection
    }

    private static let retryLadder: [TimeInterval] = [1, 2, 4, 8, 16]
    private let stableResetInterval: TimeInterval
    private let longBackgroundInterval: TimeInterval
    private let minimumLeaseValidity: TimeInterval
    private var online: Bool
    private var foreground: Bool
    private var retryIndex = 0
    private var backgroundedAt: Date?
    private var blockedReason: BlockReason?
    private var replaceWhenOnline = false
    public private(set) var state: State

    public init(
        online: Bool = true,
        foreground: Bool = true,
        stableResetInterval: TimeInterval = 30,
        longBackgroundInterval: TimeInterval = 60,
        minimumLeaseValidity: TimeInterval = 30
    ) {
        self.online = online
        self.foreground = foreground
        self.stableResetInterval = stableResetInterval
        self.longBackgroundInterval = longBackgroundInterval
        self.minimumLeaseValidity = minimumLeaseValidity
        state = online ? .disconnected : .offline
    }

    public mutating func setOnline(_ value: Bool, at now: Date) -> Command {
        online = value
        guard value else {
            state = .offline
            return .none
        }
        if let blockedReason {
            state = .blocked(blockedReason)
            return .none
        }
        if replaceWhenOnline {
            replaceWhenOnline = false
            state = .disconnected
            return foreground ? .replaceConnection : .none
        }
        state = .disconnected
        return foreground ? .connect(after: 0) : .none
    }

    public mutating func backgrounded(at now: Date) -> Command {
        foreground = false
        backgroundedAt = now
        return .none
    }

    public mutating func foregrounded(at now: Date) -> Command {
        foreground = true
        if let backgroundedAt,
           now.timeIntervalSince(backgroundedAt) >= longBackgroundInterval {
            self.backgroundedAt = nil
            state = online ? .disconnected : .offline
            if online { return .replaceConnection }
            replaceWhenOnline = true
            return .none
        }
        guard online else {
            state = .offline
            return .none
        }
        if let blockedReason {
            state = .blocked(blockedReason)
            return .none
        }
        backgroundedAt = nil
        if case let .connected(leaseExpiresAt, _) = state,
           leaseExpiresAt.timeIntervalSince(now) > minimumLeaseValidity {
            return .probeLease
        }
        state = .disconnected
        return .connect(after: 0)
    }

    public mutating func connected(leaseExpiresAt: Date, at now: Date) {
        guard online else {
            state = .offline
            return
        }
        state = .connected(leaseExpiresAt: leaseExpiresAt, establishedAt: now)
    }

    public mutating func transientFailure(at now: Date) -> Command {
        guard online, foreground else {
            state = online ? .disconnected : .offline
            return .none
        }
        if case let .connected(_, establishedAt) = state,
           now.timeIntervalSince(establishedAt) >= stableResetInterval {
            retryIndex = 0
        }
        state = .disconnected
        let delay = Self.retryLadder[min(retryIndex, Self.retryLadder.count - 1)]
        retryIndex = min(retryIndex + 1, Self.retryLadder.count - 1)
        return .connect(after: delay)
    }

    public mutating func block(_ reason: BlockReason) {
        blockedReason = reason
        state = .blocked(reason)
    }

    public mutating func credentialOrConfigurationWake(at now: Date) -> Command {
        guard online else {
            state = .offline
            return .none
        }
        blockedReason = nil
        replaceWhenOnline = false
        retryIndex = 0
        state = .disconnected
        return foreground ? .connect(after: 0) : .none
    }

    public mutating func invalidateLease() {
        if online { state = .disconnected }
    }
}
