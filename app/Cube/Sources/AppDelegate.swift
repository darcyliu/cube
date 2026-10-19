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
        setupMainMenu()
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

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag {
            openDashboard()
        }
        return true
    }

    static func ensureRegularActivation() {
        if NSApp.activationPolicy() != .regular {
            NSApp.setActivationPolicy(.regular)
        }
    }

    // MARK: - Standard Application Main Menu

    private func setupMainMenu() {
        let mainMenu = NSMenu()

        // 1. Cube Application Menu
        let appMenuItem = NSMenuItem()
        let appMenu = NSMenu(title: "Cube")
        
        let aboutItem = NSMenuItem(title: "About Cube", action: #selector(showAboutWindow), keyEquivalent: "")
        aboutItem.target = self
        appMenu.addItem(aboutItem)
        
        appMenu.addItem(NSMenuItem.separator())
        
        let prefsItem = NSMenuItem(title: "Settings...", action: #selector(openPreferences), keyEquivalent: ",")
        prefsItem.target = self
        appMenu.addItem(prefsItem)
        
        appMenu.addItem(NSMenuItem.separator())
        
        let servicesItem = NSMenuItem(title: "Services", action: nil, keyEquivalent: "")
        let servicesMenu = NSMenu(title: "Services")
        servicesItem.submenu = servicesMenu
        appMenu.addItem(servicesItem)
        NSApp.servicesMenu = servicesMenu
        
        appMenu.addItem(NSMenuItem.separator())
        appMenu.addItem(NSMenuItem(title: "Hide Cube", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h"))
        
        let hideOthers = NSMenuItem(title: "Hide Others", action: #selector(NSApplication.hideOtherApplications(_:)), keyEquivalent: "h")
        hideOthers.keyEquivalentModifierMask = [.command, .option]
        appMenu.addItem(hideOthers)
        
        appMenu.addItem(NSMenuItem(title: "Show All", action: #selector(NSApplication.unhideAllApplications(_:)), keyEquivalent: ""))
        appMenu.addItem(NSMenuItem.separator())
        appMenu.addItem(NSMenuItem(title: "Quit Cube", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        appMenuItem.submenu = appMenu
        mainMenu.addItem(appMenuItem)

        // 2. File Menu
        let fileMenuItem = NSMenuItem()
        let fileMenu = NSMenu(title: "File")
        let newVMItem = NSMenuItem(title: "New Virtual Machine...", action: #selector(openNewVM), keyEquivalent: "n")
        newVMItem.target = self
        fileMenu.addItem(newVMItem)
        fileMenu.addItem(NSMenuItem.separator())
        fileMenu.addItem(NSMenuItem(title: "Close Window", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w"))
        fileMenuItem.submenu = fileMenu
        mainMenu.addItem(fileMenuItem)

        // 3. Edit Menu (Standard clipboard & text editing key equivalents)
        let editMenuItem = NSMenuItem()
        let editMenu = NSMenu(title: "Edit")
        editMenu.addItem(NSMenuItem(title: "Undo", action: Selector(("undo:")), keyEquivalent: "z"))
        let redoItem = NSMenuItem(title: "Redo", action: Selector(("redo:")), keyEquivalent: "Z")
        editMenu.addItem(redoItem)
        editMenu.addItem(NSMenuItem.separator())
        editMenu.addItem(NSMenuItem(title: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x"))
        editMenu.addItem(NSMenuItem(title: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c"))
        editMenu.addItem(NSMenuItem(title: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v"))
        editMenu.addItem(NSMenuItem(title: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a"))
        editMenuItem.submenu = editMenu
        mainMenu.addItem(editMenuItem)

        // 4. View Menu
        let viewMenuItem = NSMenuItem()
        let viewMenu = NSMenu(title: "View")
        let refreshItem = NSMenuItem(title: "Refresh Virtual Machines", action: #selector(refreshVMs), keyEquivalent: "r")
        refreshItem.target = self
        viewMenu.addItem(refreshItem)
        viewMenuItem.submenu = viewMenu
        mainMenu.addItem(viewMenuItem)

        // 5. Window Menu
        let windowMenuItem = NSMenuItem()
        let windowMenu = NSMenu(title: "Window")
        let dashItem = NSMenuItem(title: "Dashboard", action: #selector(openDashboard), keyEquivalent: "d")
        dashItem.target = self
        windowMenu.addItem(dashItem)

        let logsItem = NSMenuItem(title: "VM Logs...", action: #selector(openVMLogs), keyEquivalent: "l")
        logsItem.target = self
        windowMenu.addItem(logsItem)

        windowMenu.addItem(NSMenuItem.separator())
        windowMenu.addItem(NSMenuItem(title: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m"))
        windowMenu.addItem(NSMenuItem(title: "Zoom", action: #selector(NSWindow.performZoom(_:)), keyEquivalent: ""))
        windowMenu.addItem(NSMenuItem.separator())
        windowMenu.addItem(NSMenuItem(title: "Bring All to Front", action: #selector(NSApplication.arrangeInFront(_:)), keyEquivalent: ""))
        windowMenuItem.submenu = windowMenu
        mainMenu.addItem(windowMenuItem)
        NSApp.windowsMenu = windowMenu

        // 6. Help Menu
        let helpMenuItem = NSMenuItem()
        let helpMenu = NSMenu(title: "Help")
        let helpDocItem = NSMenuItem(title: "Cube Documentation", action: #selector(openHelp), keyEquivalent: "?")
        helpDocItem.target = self
        helpMenu.addItem(helpDocItem)
        helpMenuItem.submenu = helpMenu
        mainMenu.addItem(helpMenuItem)
        NSApp.helpMenu = helpMenu

        NSApp.mainMenu = mainMenu
    }

    @objc func showAboutWindow() {
        AppDelegate.ensureRegularActivation()
        var options: [NSApplication.AboutPanelOptionKey: Any] = [
            .applicationVersion: "Build \(AppVersion.build)",
            .version: AppVersion.version,
            .applicationName: "Cube"
        ]
        if let creditsURL = Bundle.main.url(forResource: "Credits", withExtension: "rtf"),
           let creditsAttr = try? NSAttributedString(url: creditsURL, options: [:], documentAttributes: nil) {
            options[.credits] = creditsAttr
        }
        NSApp.orderFrontStandardAboutPanel(options: options)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc func openPreferences() {
        AppDelegate.ensureRegularActivation()
        PreferencesWindowController.shared.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc func openNewVM() {
        AppDelegate.ensureRegularActivation()
        NewVMWindowController.show()
    }

    @objc func openDashboard() {
        AppDelegate.ensureRegularActivation()
        DashboardWindowController.shared.showWindow(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc func openVMLogs() {
        AppDelegate.ensureRegularActivation()
        VMLogWindowController.show()
    }

    @objc func refreshVMs() {
        VMManager.shared.refreshVMs(completion: nil)
    }

    @objc func openHelp() {
        if let url = URL(string: "https://github.com/darcyliu/vmm") {
            NSWorkspace.shared.open(url)
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
