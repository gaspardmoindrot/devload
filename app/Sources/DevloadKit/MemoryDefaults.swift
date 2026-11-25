import Foundation

/// Defaults that live and die with the object. Tests and snapshots use it:
/// a `UserDefaults(suiteName:)` writes a plist to ~/Library/Preferences
/// that outlives the run.
public final class MemoryDefaults: UserDefaults, @unchecked Sendable {
    private var values: [String: Any] = [:]

    public init() {
        // Never written to: every write is overridden below.
        super.init(suiteName: "devload.memory")!
    }

    public override func object(forKey key: String) -> Any? { values[key] }
    public override func set(_ value: Any?, forKey key: String) { values[key] = value }
    public override func set(_ value: Int, forKey key: String) { values[key] = value }
    public override func removeObject(forKey key: String) { values[key] = nil }
    public override func integer(forKey key: String) -> Int { (values[key] as? NSNumber)?.intValue ?? 0 }
    public override func dictionaryRepresentation() -> [String: Any] { values }
}
