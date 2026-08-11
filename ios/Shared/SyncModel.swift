import Foundation

public struct Dot: Codable, Hashable, Comparable, Sendable {
    public let actorId: String
    public let counter: UInt64

    public init(actorId: String, counter: UInt64) {
        self.actorId = actorId
        self.counter = counter
    }

    public static func < (lhs: Dot, rhs: Dot) -> Bool {
        lhs.counter == rhs.counter ? lhs.actorId < rhs.actorId : lhs.counter < rhs.counter
    }
}

public struct VersionVector: Codable, Equatable, Sendable {
    public private(set) var counters: [String: UInt64]

    public init(_ counters: [String: UInt64] = [:]) {
        self.counters = counters
    }

    public func observes(_ dot: Dot) -> Bool {
        counters[dot.actorId, default: 0] >= dot.counter
    }

    public mutating func observe(_ dot: Dot) {
        counters[dot.actorId] = max(counters[dot.actorId, default: 0], dot.counter)
    }

    public mutating func merge(_ other: VersionVector) {
        for (actor, counter) in other.counters {
            counters[actor] = max(counters[actor, default: 0], counter)
        }
    }

    mutating func setCounter(_ counter: UInt64, for actor: String) {
        if counter == 0 {
            counters.removeValue(forKey: actor)
        } else {
            counters[actor] = counter
        }
    }

    public init(from decoder: Decoder) throws {
        counters = try [String: UInt64](from: decoder)
    }

    public func encode(to encoder: Encoder) throws {
        try counters.encode(to: encoder)
    }
}

public struct ContentVersion: Codable, Equatable, Sendable {
    public let dot: Dot
    public let context: VersionVector
    public let value: String

    public init(dot: Dot, context: VersionVector, value: String) {
        self.dot = dot
        self.context = context
        self.value = value
    }
}

public struct ContentRegister: Codable, Equatable, Sendable {
    public private(set) var versions: [ContentVersion]

    public init(versions: [ContentVersion] = []) {
        self.versions = versions.sorted { $0.dot < $1.dot }
    }

    public mutating func apply(_ incoming: ContentVersion) {
        guard !versions.contains(where: { $0.dot == incoming.dot }) else { return }
        guard !versions.contains(where: { $0.context.observes(incoming.dot) }) else { return }
        versions.removeAll { incoming.context.observes($0.dot) }
        versions.append(incoming)
        versions.sort { $0.dot < $1.dot }
    }

    public var hasConflict: Bool { versions.count > 1 }
    public var projectedValue: String? { versions.last?.value }
}

public enum SyncState: String, Codable, Sendable {
    case idle
    case syncing
    case synced
    case waitingForDevice
}

public struct SyncPayload: Codable, Sendable {
    public let schemaVersion: UInt16
    public let workspaceId: String
    public let frontier: VersionVector
    public let operations: [SyncOperation]

    public init(
        schemaVersion: UInt16 = 1,
        workspaceId: String,
        frontier: VersionVector,
        operations: [SyncOperation]
    ) {
        self.schemaVersion = schemaVersion
        self.workspaceId = workspaceId
        self.frontier = frontier
        self.operations = operations
    }
}

public struct SyncOperation: Codable, Equatable, Sendable {
    public let schemaVersion: UInt16
    public let workspaceId: String
    public let entityKind: String
    public let entityId: String
    public let dot: Dot
    public let mutation: Mutation

    public enum Mutation: Codable, Equatable, Sendable {
        case setMetadata(field: String, value: JSONValue)
        case setContent(context: VersionVector, value: String)
        case resolveContent(context: VersionVector, value: String)
        case delete

        private enum CodingKeys: String, CodingKey { case type, field, value, context }
        private enum Kind: String, Codable { case setMetadata, setContent, resolveContent, delete }

        public init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            switch try values.decode(Kind.self, forKey: .type) {
            case .setMetadata:
                self = .setMetadata(
                    field: try values.decode(String.self, forKey: .field),
                    value: try values.decode(JSONValue.self, forKey: .value)
                )
            case .setContent:
                self = .setContent(
                    context: try values.decode(VersionVector.self, forKey: .context),
                    value: try values.decode(String.self, forKey: .value)
                )
            case .resolveContent:
                self = .resolveContent(
                    context: try values.decode(VersionVector.self, forKey: .context),
                    value: try values.decode(String.self, forKey: .value)
                )
            case .delete:
                self = .delete
            }
        }

        public func encode(to encoder: Encoder) throws {
            var values = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .setMetadata(field, value):
                try values.encode(Kind.setMetadata, forKey: .type)
                try values.encode(field, forKey: .field)
                try values.encode(value, forKey: .value)
            case let .setContent(context, value):
                try values.encode(Kind.setContent, forKey: .type)
                try values.encode(context, forKey: .context)
                try values.encode(value, forKey: .value)
            case let .resolveContent(context, value):
                try values.encode(Kind.resolveContent, forKey: .type)
                try values.encode(context, forKey: .context)
                try values.encode(value, forKey: .value)
            case .delete:
                try values.encode(Kind.delete, forKey: .type)
            }
        }
    }
}

public enum JSONValue: Codable, Equatable, Sendable {
    case string(String), number(Double), bool(Bool), object([String: JSONValue]), array([JSONValue]), null

    public init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if value.decodeNil() { self = .null }
        else if let decoded = try? value.decode(Bool.self) { self = .bool(decoded) }
        else if let decoded = try? value.decode(Double.self) { self = .number(decoded) }
        else if let decoded = try? value.decode(String.self) { self = .string(decoded) }
        else if let decoded = try? value.decode([String: JSONValue].self) { self = .object(decoded) }
        else { self = .array(try value.decode([JSONValue].self)) }
    }

    public func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self {
        case let .string(item): try value.encode(item)
        case let .number(item): try value.encode(item)
        case let .bool(item): try value.encode(item)
        case let .object(item): try value.encode(item)
        case let .array(item): try value.encode(item)
        case .null: try value.encodeNil()
        }
    }
}
