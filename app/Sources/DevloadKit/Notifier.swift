import Foundation
import UserNotifications

/// One notification per pressure episode (the core decides when one starts).
@MainActor
public final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    private let center: UNUserNotificationCenter?

    /// Outside an .app bundle (swift run, tests) there is no notification centre.
    public override init() {
        center = Bundle.main.bundleIdentifier == nil ? nil : UNUserNotificationCenter.current()
        super.init()
        center?.delegate = self
    }

    public func requestAuthorization() {
        center?.requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    public func post(pressure: Pressure, used: UInt64, total: UInt64, biggest: Row?) {
        guard let center else { return }
        let text = Format.alert(pressure: pressure, used: used, total: total, biggest: biggest)
        let content = UNMutableNotificationContent()
        content.title = text.title
        content.body = text.body
        content.threadIdentifier = "pressure"
        content.sound = pressure == .critical ? .default : nil
        center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    /// Shown even though devload is the "active" app when the popover is open.
    public nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                                   withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .list, .sound])
    }
}
