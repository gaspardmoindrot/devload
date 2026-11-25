import ServiceManagement

public enum LoginStatus: Equatable, Sendable {
    case enabled, disabled
    /// Registered, but the user has to allow it in System Settings.
    case requiresApproval
    /// The system can't find the app (not in /Applications, or not a bundle).
    case notFound

    public var isOn: Bool { self == .enabled || self == .requiresApproval }

    /// The line shown under the toggle, if any.
    public var note: String? {
        switch self {
        case .enabled, .disabled: nil
        case .requiresApproval: "Allow devload in System Settings › General › Login Items"
        case .notFound: "Move Devload.app to /Applications first"
        }
    }
}

@MainActor
public protocol LoginItemControlling: AnyObject {
    var status: LoginStatus { get }
    func setEnabled(_ on: Bool) throws
}

/// Launch at login through SMAppService.mainApp: no plist of our own, the
/// system lists the app once in Login Items.
@MainActor
public final class LoginItem: LoginItemControlling {
    public init() {}

    public var status: LoginStatus {
        switch SMAppService.mainApp.status {
        case .enabled: .enabled
        case .requiresApproval: .requiresApproval
        case .notFound: .notFound
        default: .disabled
        }
    }

    public func setEnabled(_ on: Bool) throws {
        if on {
            try SMAppService.mainApp.register()
            if status == .requiresApproval { SMAppService.openSystemSettingsLoginItems() }
        } else {
            try SMAppService.mainApp.unregister()
        }
    }
}
