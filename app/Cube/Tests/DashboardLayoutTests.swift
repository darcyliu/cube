import AppKit

@main
struct DashboardLayoutTests {
    @MainActor
    static func main() {
        _ = NSApplication.shared
        let controller = DashboardWindowController()
        let window = controller.window!
        let root = window.contentView!
        let split = root.subviews.compactMap { $0 as? NSSplitView }.first!
        precondition(split.arrangedSubviews.count == 2)
        let sidebar = split.arrangedSubviews[0]
        let detail = split.arrangedSubviews[1]
        let sidebarScroll = sidebar.subviews.compactMap { $0 as? NSScrollView }.first!

        func checkLayout() {
            root.layoutSubtreeIfNeeded()
            precondition(!sidebar.isHidden && !split.isSubviewCollapsed(sidebar))
            precondition(sidebar.frame.width >= 199 && sidebar.frame.width <= 341)
            precondition(detail.frame.width >= 499)
            precondition(sidebar.frame.height > 300)
            precondition(abs(sidebar.frame.height - split.bounds.height) < 1)
            precondition(abs(sidebar.frame.minX) < 1)
            precondition(abs(detail.frame.minX - sidebar.frame.maxX - split.dividerThickness) < 1)
            precondition(abs(detail.frame.maxX - split.bounds.width) < 1)
            precondition(!sidebarScroll.hasHorizontalScroller)
            precondition(sidebarScroll.horizontalScrollElasticity == .none)
            precondition(sidebarScroll.documentView!.frame.width <= sidebarScroll.contentView.bounds.width + 1,
                         "Sidebar table overflows horizontally")
        }

        checkLayout()
        precondition(abs(sidebar.frame.width - 240) < 1, "Initial sidebar width")
        for width in [1000.0, 1200.0, 960.0] {
            window.setContentSize(NSSize(width: width, height: 580))
            checkLayout()
            precondition(abs(sidebar.frame.width - 240) < 1, "Resize to \(width) changed sidebar width to \(sidebar.frame.width)")
        }
        // Give the detail controls enough room to exercise the full divider range.
        window.setContentSize(NSSize(width: 1200, height: 580))
        checkLayout()
        for position in [210.0, 240.0, 300.0, 340.0] {
            split.setPosition(position, ofDividerAt: 0)
            checkLayout()
            precondition(abs(sidebar.frame.width - position) < 1, "Divider requested \(position), got \(sidebar.frame.width)")
        }
        window.setContentSize(NSSize(width: 820, height: 480))
        checkLayout()

        let commandConfig = dirForCommandTest()
        defer { try? FileManager.default.removeItem(at: commandConfig.deletingLastPathComponent()) }
        let commandVM = VMInfo(name: "display-test", state: "stopped", pid: nil, started: nil,
                               ip: nil, ssh_port: nil, vnc: "127.0.0.1:5999",
                               config: commandConfig.path, os: "FreeBSD")
        let commandController = VMCommandsSheetController(vm: commandVM)
        let commandRoot = commandController.window!.contentView!
        let descendants = allSubviews(of: commandRoot)
        let selector = descendants.compactMap { $0 as? NSSegmentedControl }.first {
            $0.segmentCount == 2 && $0.label(forSegment: 0) == "VNC" && $0.label(forSegment: 1) == "Native"
        }!
        let commandText = descendants.compactMap { $0 as? NSTextView }.first {
            $0.string.hasPrefix("cube ")
        }!
        precondition(selector.selectedSegment == 0)
        precondition(commandText.string.contains("--vnc 127.0.0.1:5999"))
        precondition(!commandText.string.contains("--gui native"))
        selector.selectedSegment = 1
        NSApp.sendAction(selector.action!, to: selector.target, from: selector)
        precondition(commandText.string.contains("--gui native"))
        precondition(!commandText.string.contains("--vnc"))
        let launchButton = descendants.compactMap { $0 as? NSButton }.first {
            $0.title == "Start with Native Display"
        }
        precondition(launchButton != nil)

        let newVMController = NewVMWindowController()
        let newVMSubviews = allSubviews(of: newVMController.window!.contentView!)
        let desktopSelector = newVMSubviews.compactMap { $0 as? NSPopUpButton }.first {
            $0.itemTitles == ["None (Server)", "XFCE", "KDE Plasma"]
        }
        precondition(desktopSelector != nil && desktopSelector!.isEnabled)

        let showPasswordButton = newVMSubviews.compactMap { $0 as? NSButton }.first {
            $0.identifier?.rawValue == "rootPasswordVisibilityButton"
        }!
        let securePasswordField = newVMSubviews.compactMap { $0 as? NSSecureTextField }.first {
            $0.identifier?.rawValue == "rootPasswordSecureField"
        }!
        let visiblePasswordField = newVMSubviews.compactMap { $0 as? NSTextField }.first {
            $0.identifier?.rawValue == "rootPasswordVisibleField"
        }!
        precondition(showPasswordButton.title == "Show")
        precondition(!securePasswordField.isHidden && visiblePasswordField.isHidden)
        NSApp.sendAction(showPasswordButton.action!, to: showPasswordButton.target, from: showPasswordButton)
        precondition(showPasswordButton.title == "Hide")
        precondition(securePasswordField.isHidden && !visiblePasswordField.isHidden)
        visiblePasswordField.stringValue = "changed-password"
        NSApp.sendAction(showPasswordButton.action!, to: showPasswordButton.target, from: showPasswordButton)
        precondition(showPasswordButton.title == "Show")
        precondition(securePasswordField.stringValue == "changed-password")

        newVMController.revealProvisioningLogs()
        newVMController.window!.contentView!.layoutSubtreeIfNeeded()
        let provisioningLabel = allSubviews(of: newVMController.window!.contentView!).compactMap { $0 as? NSTextField }.first {
            $0.stringValue == "PROVISIONING LOGS"
        }!
        let logCard = provisioningLabel.superview!
        let formScrollView = allSubviews(of: newVMController.window!.contentView!).compactMap { $0 as? NSScrollView }.first {
            guard let documentView = $0.documentView else { return false }
            return logCard.isDescendant(of: documentView)
        }!
        let logRect = logCard.convert(logCard.bounds, to: formScrollView.documentView)
        precondition(formScrollView.documentVisibleRect.intersects(logRect), "Provisioning logs should scroll into view")
        print("PASS: dashboard layout and command display selector")
    }

    @MainActor
    static func allSubviews(of view: NSView) -> [NSView] {
        view.subviews + view.subviews.flatMap { allSubviews(of: $0) }
    }

    static func dirForCommandTest() -> URL {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("cube-command-layout-\(UUID().uuidString)")
        try! FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let config = directory.appendingPathComponent("display-test.json")
        let json: [String: Any] = [
            "name": "display-test",
            "boot": ["mode": "freebsd", "kernel": "kernel"],
            "network": ["mode": "none"],
            "disks": ["disk.img"],
            "vnc": ["bind": "127.0.0.1:5999", "display": "1280x800"]
        ]
        try! JSONSerialization.data(withJSONObject: json).write(to: config)
        return config
    }
}
