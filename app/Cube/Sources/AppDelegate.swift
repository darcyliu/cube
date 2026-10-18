import Cocoa

struct AppVersion {
    static var version: String {
        return Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.1.0"
    }
    static var build: String {
        return Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "1"
    }
    static var displayString: String {
        return "v\(version) (Build \(build))"
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {
    private var menuController: MenuController?

    func applicationDidFinishLaunching(_ aNotification: Notification) {
        menuController = MenuController()
        
        // Auto-start network service if configured and not already running
        if ServiceManager.shared.autoStartService && !ServiceManager.shared.isRunning {
            ServiceManager.shared.startService { success, error in
                if !success, let error = error {
                    let alert = NSAlert()
                    alert.messageText = "Network Service Could Not Start"
                    alert.informativeText = error
                    alert.runModal()
                }
            }
        }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        DispatchQueue.main.async {
            ServiceManager.shared.stopService { success, error in
                if let error = error {
                    let alert = NSAlert()
                    alert.messageText = "Could Not Stop Network Service"
                    alert.informativeText = error
                    alert.runModal()
                }
                sender.reply(toApplicationShouldTerminate: success)
            }
        }
        return .terminateLater
    }
}
