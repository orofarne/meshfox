import AppKit
import Foundation
import UserNotifications

/// Native notification for "a run is waiting for a form" — the
/// `form_awaiting` kind of `watcher_protocol.rs`'s `Message::Notify`. `form_resolved` withdraws it again. Every
/// other kind is ignored for now. Clicking the notification opens the
/// canvas at the form's node.
///
/// `UNUserNotificationCenter` only works for a bundled app (it traps in an
/// unbundled `swift run`), so an unbundled daemon falls back to
/// `osascript`'s `display notification` (no click action there).
final class FormNotifier: NSObject, UNUserNotificationCenterDelegate {
    static let formAwaiting = "form_awaiting"
    static let formResolved = "form_resolved"
    private static let canvasKey = "canvas_path"
    private static let nodeKey = "node_id"

    private let open: (String, String?) -> Void
    private let bundled: Bool

    init(open: @escaping (String, String?) -> Void) {
        self.open = open
        self.bundled = Bundle.main.bundleURL.pathExtension == "app"
        super.init()
        if bundled {
            let center = UNUserNotificationCenter.current()
            center.delegate = self
            center.requestAuthorization(options: [.alert, .sound]) { _, error in
                if let error {
                    FileHandle.standardError.write("meshfox-daemon: notification authorization failed: \(error)\n".data(using: .utf8)!)
                }
            }
        }
    }

    private static func identifier(_ canvasPath: String, _ event: CoordinatorEvent) -> String {
        "form-\(canvasPath)-\(event.nodeId ?? "")-\(event.block ?? "form")"
    }

    func handle(canvasPath: String, event: CoordinatorEvent) {
        if event.kind == Self.formResolved {
            // Answered or cancelled elsewhere: the banner is stale now.
            if bundled {
                let center = UNUserNotificationCenter.current()
                let id = Self.identifier(canvasPath, event)
                center.removeDeliveredNotifications(withIdentifiers: [id])
                center.removePendingNotificationRequests(withIdentifiers: [id])
            }
            return
        }
        guard event.kind == Self.formAwaiting else { return }
        let canvas = ((canvasPath as NSString).lastPathComponent as NSString).deletingPathExtension
        let title = "Meshfox is waiting for you"
        let form = event.block ?? "form"
        var body = "\(canvas) · \(form)"
        if let message = event.message, !message.isEmpty { body += ": \(message)" }

        if bundled {
            let content = UNMutableNotificationContent()
            content.title = title
            content.body = body
            content.sound = .default
            content.userInfo = [Self.canvasKey: canvasPath, Self.nodeKey: event.nodeId ?? ""]
            // One notification per parked form: a repeat replaces it.
            let id = Self.identifier(canvasPath, event)
            UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: id, content: content, trigger: nil))
        } else {
            Self.osascript(title: title, body: body)
        }
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let info = response.notification.request.content.userInfo
        if let path = info[Self.canvasKey] as? String {
            let node = (info[Self.nodeKey] as? String).flatMap { $0.isEmpty ? nil : $0 }
            open(path, node)
        }
        completionHandler()
    }

    /// Show banners even while the daemon is the active app.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }

    private static func osascript(title: String, body: String) {
        func quoted(_ s: String) -> String {
            "\"" + s.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") + "\""
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
        process.arguments = ["-e", "display notification \(quoted(body)) with title \(quoted(title))"]
        try? process.run()
    }
}
