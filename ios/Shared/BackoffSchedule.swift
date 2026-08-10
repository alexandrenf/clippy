import Foundation

public struct BackoffSchedule: Sendable {
    public enum Visibility: Sendable { case foreground, hidden }

    private static let foregroundBurst: [TimeInterval] = [0, 0.25, 1, 3]
    private var visibility: Visibility
    private var failures = 0
    private var burstIndex = 0

    public init(visibility: Visibility) {
        self.visibility = visibility
    }

    public mutating func shown() {
        visibility = .foreground
        failures = 0
        burstIndex = 0
    }

    public mutating func hidden() {
        visibility = .hidden
        burstIndex = Self.foregroundBurst.count
    }

    public mutating func succeeded() {
        failures = 0
        burstIndex = Self.foregroundBurst.count
    }

    public mutating func failed() {
        failures = min(failures + 1, 30)
        burstIndex = Self.foregroundBurst.count
    }

    public mutating func nextDelay(jitterUnit: Double, hasLocalOperations: Bool) -> TimeInterval {
        if hasLocalOperations { return 0 }
        if visibility == .foreground, burstIndex < Self.foregroundBurst.count {
            defer { burstIndex += 1 }
            return Self.foregroundBurst[burstIndex]
        }
        let base: TimeInterval
        switch visibility {
        case .foreground:
            base = min(pow(2, Double(min(failures, 4))), 15)
        case .hidden:
            base = min(30 * pow(2, Double(min(failures, 5))), 900)
        }
        return base * (0.8 + min(max(jitterUnit, 0), 1) * 0.4)
    }
}
