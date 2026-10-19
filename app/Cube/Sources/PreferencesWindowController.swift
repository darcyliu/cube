import Foundation
import AppKit
import ServiceManagement
import UniformTypeIdentifiers

private class FlippedContainerView: NSView {
    override var isFlipped: Bool { return true }
}

private class FlippedBox: NSBox {
    override var isFlipped: Bool { return true }
}

struct HostNetworkInfo {
    let interface: String
    let ipv4: String?
    let ipv6: String?
}

func detectHostNetworkInfo() -> HostNetworkInfo {
    var primaryIface = "en0"
    var ipv4Addr: String? = nil
    var ipv6Addr: String? = nil
    
    var ifap: UnsafeMutablePointer<ifaddrs>?
    guard getifaddrs(&ifap) == 0, let first = ifap else {
        return HostNetworkInfo(interface: primaryIface, ipv4: nil, ipv6: nil)
    }
    defer { freeifaddrs(ifap) }
    
    var cursor: UnsafeMutablePointer<ifaddrs>? = first
    while let ptr = cursor {
        let name = String(cString: ptr.pointee.ifa_name)
        let flags = Int32(ptr.pointee.ifa_flags)
        let isUp = (flags & IFF_UP) != 0
        let isLoopback = (flags & IFF_LOOPBACK) != 0
        
        // Scan active physical en* interfaces
        if isUp && !isLoopback && name.starts(with: "en"), let sa = ptr.pointee.ifa_addr {
            let family = sa.pointee.sa_family
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            if family == UInt8(AF_INET) && ipv4Addr == nil {
                if getnameinfo(sa, socklen_t(sa.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 {
                    ipv4Addr = String(cString: host)
                    primaryIface = name
                }
            } else if family == UInt8(AF_INET6) {
                if getnameinfo(sa, socklen_t(sa.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 {
                    let addr = String(cString: host)
                    if !addr.starts(with: "fe80:") {
                        if ipv6Addr == nil || ipv6Addr!.starts(with: "fe80:") {
                            ipv6Addr = addr
                        }
                    } else if ipv6Addr == nil {
                        ipv6Addr = addr
                    }
                }
            }
        }
        cursor = ptr.pointee.ifa_next
    }
    
    return HostNetworkInfo(interface: primaryIface, ipv4: ipv4Addr, ipv6: ipv6Addr)
}

struct PreferenceTabItem {
    let id: String
    let title: String
    let iconName: String
}

struct DetectedSSHKey {
    let path: String
    let keyType: String
    let keyContent: String
    let fingerprint: String
}

class PreferencesWindowController: NSWindowController, NSTableViewDataSource, NSTableViewDelegate {
    static let shared = PreferencesWindowController()
    
    // Sidebar Tabs (General is first)
    private let tabs: [PreferenceTabItem] = [
        PreferenceTabItem(id: "general", title: "General", iconName: "gearshape"),
        PreferenceTabItem(id: "network", title: "Network", iconName: "network"),
        PreferenceTabItem(id: "storage", title: "Storage", iconName: "internaldrive"),
        PreferenceTabItem(id: "ssh", title: "SSH Keys", iconName: "key"),
        PreferenceTabItem(id: "about", title: "About", iconName: "info.circle")
    ]
    
    private var sidebarTableView: NSTableView!
    private var contentContainerView: NSView!
    private var tabViews: [String: NSView] = [:]
    private var currentTabId: String = "general"
    
    // General Tab Controls
    private var autoStartCheckbox: NSButton!
    private var launchAtLoginCheckbox: NSButton!
    private var terminalPopUp: NSPopUpButton!

    // Network Tab Controls
    private var radioAppleDefault: NSButton!
    private var radioCustomSubnet: NSButton!
    private var radioBridged: NSButton!
    private var customSubnetField: NSTextField!
    private var interfacePopUp: NSPopUpButton!
    private var hostIPv4Label: NSTextField!
    private var hostIPv6Label: NSTextField!
    private var vmGatewayLabel: NSTextField!
    private var socketLabel: NSTextField!
    private var lastIPv4: String = ""
    private var lastIPv6: String = ""
    
    // Storage Tab Controls
    private var vmsDirField: NSTextField!
    private var storageFreeSpaceLabel: NSTextField!
    private var storageVMCountLabel: NSTextField!
    
    // SSH Keys Tab Controls
    private var sshKeyStatusDot: NSTextField!
    private var sshKeyPathLabel: NSTextField!
    private var sshKeyTypeLabel: NSTextField!
    private var sshKeyPreviewField: NSTextField!
    private var detectedKey: DetectedSSHKey?

    convenience init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 730, height: 520),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.title = "Cube Preferences — \(AppVersion.displayString)"
        window.center()
        window.isReleasedWhenClosed = false
        
        self.init(window: window)
        setupUI()
    }
    
    private func setupUI() {
        guard let window = self.window else { return }
        
        let mainContainer = NSView(frame: window.contentView!.bounds)
        mainContainer.autoresizingMask = [.width, .height]
        window.contentView = mainContainer
        
        let sidebarWidth: CGFloat = 175
        let totalHeight: CGFloat = mainContainer.bounds.height
        let contentWidth = mainContainer.bounds.width - sidebarWidth - 1
        
        // 1. Left Sidebar View
        let sidebarContainer = NSView(frame: NSRect(x: 0, y: 0, width: sidebarWidth, height: totalHeight))
        sidebarContainer.autoresizingMask = [.height]
        
        let sidebarScrollView = NSScrollView(frame: NSRect(x: 0, y: 0, width: sidebarWidth, height: totalHeight))
        sidebarScrollView.autoresizingMask = [.width, .height]
        sidebarScrollView.hasVerticalScroller = false
        sidebarScrollView.borderType = .noBorder
        sidebarScrollView.drawsBackground = true
        sidebarScrollView.backgroundColor = NSColor.controlBackgroundColor
        
        sidebarTableView = NSTableView(frame: sidebarScrollView.bounds)
        sidebarTableView.headerView = nil
        sidebarTableView.rowHeight = 34
        if #available(macOS 11.0, *) {
            sidebarTableView.style = .sourceList
        } else {
            sidebarTableView.selectionHighlightStyle = .sourceList
        }
        sidebarTableView.backgroundColor = NSColor.controlBackgroundColor
        sidebarTableView.autoresizingMask = [.width, .height]
        
        let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("PrefTabCol"))
        col.width = sidebarWidth
        sidebarTableView.addTableColumn(col)
        
        sidebarTableView.dataSource = self
        sidebarTableView.delegate = self
        sidebarScrollView.documentView = sidebarTableView
        sidebarContainer.addSubview(sidebarScrollView)
        mainContainer.addSubview(sidebarContainer)
        
        // 2. Vertical Divider line
        let divider = NSBox(frame: NSRect(x: sidebarWidth, y: 0, width: 1, height: totalHeight))
        divider.boxType = .separator
        divider.autoresizingMask = [.height]
        mainContainer.addSubview(divider)
        
        // 3. Right Content Container (Full Height, per-tab footers)
        contentContainerView = NSView(frame: NSRect(x: sidebarWidth + 1, y: 0, width: contentWidth, height: totalHeight))
        contentContainerView.autoresizingMask = [.width, .height]
        mainContainer.addSubview(contentContainerView)
        
        // 4. Pre-build all Tab Views
        tabViews["general"] = buildGeneralTabView()
        tabViews["network"] = buildNetworkTabView()
        tabViews["storage"] = buildStorageTabView()
        tabViews["ssh"] = buildSSHKeysTabView()
        tabViews["about"] = buildAboutTabView()
        
        // Select initial tab: General
        sidebarTableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        switchTab(id: "general")
    }
    
    // MARK: - Tab Views Builders
    
    private func buildGeneralTabView() -> NSView {
        let view = FlippedContainerView(frame: NSRect(x: 0, y: 0, width: 554, height: 520))
        let margin: CGFloat = 24
        let contentWidth: CGFloat = 506
        var y: CGFloat = 22
        
        let header = NSTextField(labelWithString: "General & Automation")
        header.font = NSFont.boldSystemFont(ofSize: 15)
        header.frame = NSRect(x: margin, y: y, width: contentWidth, height: 22)
        view.addSubview(header)
        y += 28
        
        // Startup options card
        let startupBox = FlippedBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 86))
        startupBox.boxType = .custom
        startupBox.cornerRadius = 6
        startupBox.borderWidth = 1
        startupBox.borderColor = .separatorColor
        startupBox.fillColor = NSColor.windowBackgroundColor
        
        autoStartCheckbox = NSButton(checkboxWithTitle: "Automatically start Cube network service on launch", target: nil, action: nil)
        autoStartCheckbox.font = NSFont.systemFont(ofSize: 13)
        autoStartCheckbox.frame = NSRect(x: 14, y: 14, width: contentWidth - 28, height: 22)
        startupBox.addSubview(autoStartCheckbox)
        
        launchAtLoginCheckbox = NSButton(checkboxWithTitle: "Launch Cube menu bar application at macOS login", target: nil, action: nil)
        launchAtLoginCheckbox.font = NSFont.systemFont(ofSize: 13)
        launchAtLoginCheckbox.frame = NSRect(x: 14, y: 46, width: contentWidth - 28, height: 22)
        startupBox.addSubview(launchAtLoginCheckbox)
        
        view.addSubview(startupBox)
        y += 98
        
        // Preferred Terminal Card
        let termBox = FlippedBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 72))
        termBox.boxType = .custom
        termBox.cornerRadius = 6
        termBox.borderWidth = 1
        termBox.borderColor = .separatorColor
        termBox.fillColor = NSColor.windowBackgroundColor
        
        let termLabel = NSTextField(labelWithString: "Preferred Terminal Application:")
        termLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        termLabel.textColor = .secondaryLabelColor
        termLabel.frame = NSRect(x: 12, y: 10, width: contentWidth - 24, height: 16)
        termBox.addSubview(termLabel)
        
        terminalPopUp = NSPopUpButton(frame: NSRect(x: 10, y: 32, width: 260, height: 26), pullsDown: false)
        terminalPopUp.target = self
        terminalPopUp.action = #selector(terminalPopUpChanged(_:))
        termBox.addSubview(terminalPopUp)
        
        let termHint = NSTextField(wrappingLabelWithString: "Used when opening SSH terminal sessions from the dashboard and menu bar.")
        termHint.font = NSFont.systemFont(ofSize: 11)
        termHint.textColor = .tertiaryLabelColor
        termHint.frame = NSRect(x: 280, y: 30, width: contentWidth - 290, height: 32)
        termBox.addSubview(termHint)
        
        view.addSubview(termBox)
        y += 84
        
        // Keyboard Shortcuts header
        let shortcutsHeader = NSTextField(labelWithString: "Global Keyboard Shortcuts")
        shortcutsHeader.font = NSFont.boldSystemFont(ofSize: 13)
        shortcutsHeader.frame = NSRect(x: margin, y: y, width: contentWidth, height: 18)
        view.addSubview(shortcutsHeader)
        y += 24
        
        let scBox = FlippedBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 110))
        scBox.boxType = .custom
        scBox.cornerRadius = 6
        scBox.borderWidth = 1
        scBox.borderColor = .separatorColor
        scBox.fillColor = NSColor.windowBackgroundColor
        
        var scY: CGFloat = 10
        _ = addShortcutRow(action: "Open Dashboard:", shortcut: "⌘ D", in: scBox, y: scY)
        scY += 24
        _ = addShortcutRow(action: "New Virtual Machine:", shortcut: "⌘ N", in: scBox, y: scY)
        scY += 24
        _ = addShortcutRow(action: "View Virtual Machine Logs:", shortcut: "⌘ L", in: scBox, y: scY)
        scY += 24
        _ = addShortcutRow(action: "Open Preferences:", shortcut: "⌘ ,", in: scBox, y: scY)
        
        view.addSubview(scBox)
        
        // Per-tab action footer
        let footer = buildTabFooter(
            width: 554,
            y: 474,
            note: "Applies to current user session.",
            cancelTitle: "Cancel",
            cancelAction: #selector(revertGeneralClicked),
            applyTitle: "Apply",
            applyAction: #selector(applyGeneralClicked(_:))
        )
        view.addSubview(footer)
        return view
    }
    
    private func buildNetworkTabView() -> NSView {
        let view = FlippedContainerView(frame: NSRect(x: 0, y: 0, width: 554, height: 520))
        let margin: CGFloat = 24
        let contentWidth: CGFloat = 506
        var y: CGFloat = 20
        
        let header = NSTextField(labelWithString: "Network Operation Mode")
        header.font = NSFont.boldSystemFont(ofSize: 15)
        header.frame = NSRect(x: margin, y: y, width: contentWidth, height: 22)
        view.addSubview(header)
        y += 28
        
        // Radio 1: Apple Default NAT
        radioAppleDefault = NSButton(radioButtonWithTitle: "Shared NAT — Apple Default (Recommended)", target: self, action: #selector(radioChanged))
        radioAppleDefault.font = NSFont.systemFont(ofSize: 13, weight: .medium)
        radioAppleDefault.frame = NSRect(x: margin, y: y, width: contentWidth, height: 20)
        view.addSubview(radioAppleDefault)
        y += 22
        
        let appleDefaultHint = NSTextField(wrappingLabelWithString: "Uses Apple's native subnet (192.168.64.0/24). Runs 100% unprivileged without root or sudo.")
        appleDefaultHint.font = NSFont.systemFont(ofSize: 11)
        appleDefaultHint.textColor = .secondaryLabelColor
        appleDefaultHint.frame = NSRect(x: margin + 20, y: y, width: contentWidth - 20, height: 28)
        view.addSubview(appleDefaultHint)
        y += 32
        
        // Radio 2: Custom Subnet NAT
        radioCustomSubnet = NSButton(radioButtonWithTitle: "Shared NAT — Custom Subnet", target: self, action: #selector(radioChanged))
        radioCustomSubnet.font = NSFont.systemFont(ofSize: 13)
        radioCustomSubnet.frame = NSRect(x: margin, y: y, width: 220, height: 20)
        view.addSubview(radioCustomSubnet)
        
        customSubnetField = NSTextField(frame: NSRect(x: margin + 225, y: y - 1, width: 170, height: 22))
        customSubnetField.placeholderString = "192.168.18.0/24"
        customSubnetField.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        view.addSubview(customSubnetField)
        y += 22
        
        let customSubnetHint = NSTextField(wrappingLabelWithString: "Specify an RFC1918 CIDR. Requires a separately administered root service; unavailable in Cube.")
        customSubnetHint.font = NSFont.systemFont(ofSize: 11)
        customSubnetHint.textColor = .secondaryLabelColor
        customSubnetHint.frame = NSRect(x: margin + 20, y: y, width: contentWidth - 20, height: 28)
        view.addSubview(customSubnetHint)
        y += 32
        
        // Radio 3: Bridged LAN
        radioBridged = NSButton(radioButtonWithTitle: "Bridged LAN (Physical Interface)", target: self, action: #selector(radioChanged))
        radioBridged.font = NSFont.systemFont(ofSize: 13)
        radioBridged.frame = NSRect(x: margin, y: y, width: 235, height: 20)
        view.addSubview(radioBridged)
        
        interfacePopUp = NSPopUpButton(frame: NSRect(x: margin + 240, y: y - 2, width: 150, height: 24), pullsDown: false)
        for iface in detectInterfaces() {
            interfacePopUp.addItem(withTitle: iface)
        }
        view.addSubview(interfacePopUp)
        y += 22
        
        let bridgedHint = NSTextField(wrappingLabelWithString: "Attaches guests to the physical network for external DHCP address assignment.")
        bridgedHint.font = NSFont.systemFont(ofSize: 11)
        bridgedHint.textColor = .secondaryLabelColor
        bridgedHint.frame = NSRect(x: margin + 20, y: y, width: contentWidth - 20, height: 28)
        view.addSubview(bridgedHint)
        y += 34
        
        // Card: Diagnostics & Host Information
        let diagBox = FlippedBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 114))
        diagBox.boxType = .custom
        diagBox.cornerRadius = 6
        diagBox.borderWidth = 1
        diagBox.borderColor = .separatorColor
        diagBox.fillColor = NSColor.windowBackgroundColor
        
        var dy: CGFloat = 10
        hostIPv4Label = addDiagRow(label: "Host IPv4 (en0):", value: "Detecting...", in: diagBox, y: dy, copyAction: #selector(copyIPv4Clicked))
        dy += 24
        hostIPv6Label = addDiagRow(label: "Host IPv6:", value: "Detecting...", in: diagBox, y: dy, copyAction: #selector(copyIPv6Clicked))
        dy += 24
        vmGatewayLabel = addDiagRow(label: "VM Gateway:", value: "192.168.64.1", in: diagBox, y: dy, copyAction: nil)
        dy += 24
        socketLabel = addDiagRow(label: "Daemon Socket:", value: ServiceManager.shared.defaultSocketPath, in: diagBox, y: dy, copyAction: #selector(copySocketClicked))
        
        view.addSubview(diagBox)
        
        // Per-tab action footer
        let footer = buildTabFooter(
            width: 554,
            y: 474,
            note: "Requires service restart.",
            cancelTitle: "Cancel",
            cancelAction: #selector(revertNetworkClicked),
            applyTitle: "Apply & Restart",
            applyAction: #selector(applyNetworkClicked(_:))
        )
        view.addSubview(footer)
        return view
    }
    
    private func buildStorageTabView() -> NSView {
        let view = FlippedContainerView(frame: NSRect(x: 0, y: 0, width: 554, height: 520))
        let margin: CGFloat = 24
        let contentWidth: CGFloat = 506
        var y: CGFloat = 20
        
        let header = NSTextField(labelWithString: "Virtual Machine Storage")
        header.font = NSFont.boldSystemFont(ofSize: 15)
        header.frame = NSRect(x: margin, y: y, width: contentWidth, height: 22)
        view.addSubview(header)
        y += 26
        
        let desc = NSTextField(wrappingLabelWithString: "Select the primary folder for storing virtual disk images (rootfs.img), hardware configs, and VM state directories. External SSDs (Thunderbolt / USB4) are fully supported.")
        desc.font = NSFont.systemFont(ofSize: 12)
        desc.textColor = .secondaryLabelColor
        desc.frame = NSRect(x: margin, y: y, width: contentWidth, height: 36)
        view.addSubview(desc)
        y += 44
        
        // Storage path card
        let pathBox = FlippedBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 72))
        pathBox.boxType = .custom
        pathBox.cornerRadius = 6
        pathBox.borderWidth = 1
        pathBox.borderColor = .separatorColor
        pathBox.fillColor = NSColor.windowBackgroundColor
        
        let pathLabel = NSTextField(labelWithString: "Storage Directory:")
        pathLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        pathLabel.textColor = .secondaryLabelColor
        pathLabel.frame = NSRect(x: 12, y: 10, width: 200, height: 16)
        pathBox.addSubview(pathLabel)
        
        vmsDirField = NSTextField(frame: NSRect(x: 12, y: 32, width: contentWidth - 235, height: 24))
        vmsDirField.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        vmsDirField.placeholderString = "~/Cube"
        pathBox.addSubview(vmsDirField)
        
        let chooseBtn = NSButton(title: "Choose...", target: self, action: #selector(chooseVMsDirClicked))
        chooseBtn.bezelStyle = .rounded
        chooseBtn.frame = NSRect(x: contentWidth - 215, y: 30, width: 78, height: 26)
        pathBox.addSubview(chooseBtn)
        
        let revealBtn = NSButton(title: "Reveal", target: self, action: #selector(revealVMsDirClicked))
        revealBtn.bezelStyle = .rounded
        revealBtn.frame = NSRect(x: contentWidth - 134, y: 30, width: 66, height: 26)
        pathBox.addSubview(revealBtn)
        
        let resetBtn = NSButton(title: "Reset", target: self, action: #selector(resetVMsDirClicked))
        resetBtn.bezelStyle = .rounded
        resetBtn.frame = NSRect(x: contentWidth - 66, y: 30, width: 56, height: 26)
        pathBox.addSubview(resetBtn)
        
        view.addSubview(pathBox)
        y += 88
        
        // Storage Stats card
        let statsBox = FlippedBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 84))
        statsBox.boxType = .custom
        statsBox.cornerRadius = 6
        statsBox.borderWidth = 1
        statsBox.borderColor = .separatorColor
        statsBox.fillColor = NSColor.windowBackgroundColor
        
        var sy: CGFloat = 10
        storageFreeSpaceLabel = addInfoRow(label: "Volume Free Space:", value: "Calculating...", in: statsBox, y: sy)
        sy += 24
        storageVMCountLabel = addInfoRow(label: "Configured Virtual Machines:", value: "0 VMs", in: statsBox, y: sy)
        sy += 24
        _ = addInfoRow(label: "Image Format:", value: "Raw Sparse Disk Image (*.raw / *.img)", in: statsBox, y: sy)
        
        view.addSubview(statsBox)
        
        // Per-tab action footer
        let footer = buildTabFooter(
            width: 554,
            y: 474,
            note: "VMs are loaded dynamically from this location.",
            cancelTitle: "Cancel",
            cancelAction: #selector(revertStorageClicked),
            applyTitle: "Apply",
            applyAction: #selector(applyStorageClicked(_:))
        )
        view.addSubview(footer)
        return view
    }
    
    private func buildSSHKeysTabView() -> NSView {
        let view = FlippedContainerView(frame: NSRect(x: 0, y: 0, width: 554, height: 520))
        let margin: CGFloat = 24
        let contentWidth: CGFloat = 506
        var y: CGFloat = 20
        
        let header = NSTextField(labelWithString: "SSH Key Management")
        header.font = NSFont.boldSystemFont(ofSize: 15)
        header.frame = NSRect(x: margin, y: y, width: contentWidth, height: 22)
        view.addSubview(header)
        y += 26
        
        let desc = NSTextField(wrappingLabelWithString: "Cube automatically detects your public SSH keys to provision unattended root logins. You can preview, copy, or choose a custom key.")
        desc.font = NSFont.systemFont(ofSize: 12)
        desc.textColor = .secondaryLabelColor
        desc.frame = NSRect(x: margin, y: y, width: contentWidth, height: 36)
        view.addSubview(desc)
        y += 44
        
        // Detected Key Card
        let keyBox = FlippedBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 154))
        keyBox.boxType = .custom
        keyBox.cornerRadius = 6
        keyBox.borderWidth = 1
        keyBox.borderColor = .separatorColor
        keyBox.fillColor = NSColor.windowBackgroundColor
        
        sshKeyStatusDot = NSTextField(labelWithString: "●")
        sshKeyStatusDot.font = NSFont.boldSystemFont(ofSize: 12)
        sshKeyStatusDot.frame = NSRect(x: 12, y: 10, width: 14, height: 18)
        keyBox.addSubview(sshKeyStatusDot)
        
        sshKeyPathLabel = NSTextField(labelWithString: "~/.ssh/id_ed25519.pub")
        sshKeyPathLabel.font = NSFont.systemFont(ofSize: 12, weight: .semibold)
        sshKeyPathLabel.frame = NSRect(x: 28, y: 10, width: contentWidth - 40, height: 18)
        keyBox.addSubview(sshKeyPathLabel)
        
        sshKeyTypeLabel = NSTextField(labelWithString: "Type: ED25519 (256-bit elliptic curve)")
        sshKeyTypeLabel.font = NSFont.systemFont(ofSize: 11)
        sshKeyTypeLabel.textColor = .secondaryLabelColor
        sshKeyTypeLabel.frame = NSRect(x: 28, y: 30, width: contentWidth - 40, height: 16)
        keyBox.addSubview(sshKeyTypeLabel)
        
        // Monospace key preview box
        sshKeyPreviewField = NSTextField(wrappingLabelWithString: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5...")
        sshKeyPreviewField.font = NSFont.monospacedSystemFont(ofSize: 10, weight: .regular)
        sshKeyPreviewField.textColor = .labelColor
        sshKeyPreviewField.frame = NSRect(x: 12, y: 52, width: contentWidth - 24, height: 55)
        sshKeyPreviewField.isSelectable = true
        keyBox.addSubview(sshKeyPreviewField)
        
        // Buttons
        let copyKeyBtn = NSButton(title: "Copy Public Key", target: self, action: #selector(copySSHKeyClicked(_:)))
        copyKeyBtn.bezelStyle = .rounded
        copyKeyBtn.font = NSFont.systemFont(ofSize: 11)
        copyKeyBtn.frame = NSRect(x: 10, y: 116, width: 120, height: 26)
        keyBox.addSubview(copyKeyBtn)
        
        let revealKeyBtn = NSButton(title: "Reveal in Finder", target: self, action: #selector(revealSSHKeyClicked))
        revealKeyBtn.bezelStyle = .rounded
        revealKeyBtn.font = NSFont.systemFont(ofSize: 11)
        revealKeyBtn.frame = NSRect(x: 134, y: 116, width: 120, height: 26)
        keyBox.addSubview(revealKeyBtn)
        
        let chooseKeyBtn = NSButton(title: "Choose Other...", target: self, action: #selector(chooseSSHKeyClicked))
        chooseKeyBtn.bezelStyle = .rounded
        chooseKeyBtn.font = NSFont.systemFont(ofSize: 11)
        chooseKeyBtn.frame = NSRect(x: 258, y: 116, width: 115, height: 26)
        keyBox.addSubview(chooseKeyBtn)
        
        let generateKeyBtn = NSButton(title: "Generate Key", target: self, action: #selector(generateSSHKeyClicked))
        generateKeyBtn.bezelStyle = .rounded
        generateKeyBtn.font = NSFont.systemFont(ofSize: 11)
        generateKeyBtn.frame = NSRect(x: 377, y: 116, width: 105, height: 26)
        keyBox.addSubview(generateKeyBtn)
        
        view.addSubview(keyBox)
        y += 172
        
        // Security note
        let noteBox = FlippedBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 75))
        noteBox.boxType = .custom
        noteBox.cornerRadius = 6
        noteBox.borderWidth = 1
        noteBox.borderColor = .separatorColor
        noteBox.fillColor = NSColor.windowBackgroundColor
        
        let noteIcon = NSImageView(frame: NSRect(x: 12, y: 12, width: 20, height: 20))
        let heroConfig = NSImage.SymbolConfiguration(pointSize: 18, weight: .regular)
        if let lockImg = NSImage(systemSymbolName: "lock.shield", accessibilityDescription: nil)?.withSymbolConfiguration(heroConfig) {
            noteIcon.image = lockImg
            noteIcon.contentTintColor = .systemBlue
        }
        noteBox.addSubview(noteIcon)
        
        let noteText = NSTextField(wrappingLabelWithString: "Security Assurance: Private keys are never read, stored, or transmitted by Cube. Only the public key (.pub) is placed inside /root/.ssh/authorized_keys on the virtual machine for port-forwarded SSH login.")
        noteText.font = NSFont.systemFont(ofSize: 11)
        noteText.textColor = .secondaryLabelColor
        noteText.frame = NSRect(x: 40, y: 10, width: contentWidth - 52, height: 55)
        noteBox.addSubview(noteText)
        
        view.addSubview(noteBox)
        
        // Bottom information bar
        let footer = FlippedContainerView(frame: NSRect(x: 0, y: 474, width: 554, height: 46))
        footer.autoresizingMask = [.width, .minYMargin]
        let sep = NSBox(frame: NSRect(x: 0, y: 0, width: 554, height: 1))
        sep.boxType = .separator
        sep.autoresizingMask = [.width]
        footer.addSubview(sep)
        
        let footerNote = NSTextField(labelWithString: "Public key is automatically injected into new VMs during unattended install.")
        footerNote.font = NSFont.systemFont(ofSize: 11)
        footerNote.textColor = .secondaryLabelColor
        footerNote.frame = NSRect(x: 24, y: 14, width: 506, height: 18)
        footer.addSubview(footerNote)
        
        view.addSubview(footer)
        return view
    }
    
    private func buildAboutTabView() -> NSView {
        let view = FlippedContainerView(frame: NSRect(x: 0, y: 0, width: 554, height: 520))
        let margin: CGFloat = 24
        let contentWidth: CGFloat = 506
        var y: CGFloat = 20
        
        let iconView = NSImageView(frame: NSRect(x: margin, y: y, width: 48, height: 48))
        if let appIcon = NSApp.applicationIconImage {
            iconView.image = appIcon
        } else {
            let heroConfig = NSImage.SymbolConfiguration(pointSize: 36, weight: .regular)
            if let fallback = NSImage(systemSymbolName: "cube.fill", accessibilityDescription: nil)?.withSymbolConfiguration(heroConfig) {
                iconView.image = fallback
                iconView.contentTintColor = .systemBlue
            }
        }
        view.addSubview(iconView)
        
        let appTitle = NSTextField(labelWithString: "Cube")
        appTitle.font = NSFont.boldSystemFont(ofSize: 19)
        appTitle.frame = NSRect(x: margin + 62, y: y + 2, width: 250, height: 24)
        view.addSubview(appTitle)
        
        let versionLabel = NSTextField(labelWithString: "Version \(AppVersion.displayString)")
        versionLabel.font = NSFont.systemFont(ofSize: 12)
        versionLabel.textColor = .secondaryLabelColor
        versionLabel.frame = NSRect(x: margin + 62, y: y + 26, width: 250, height: 18)
        view.addSubview(versionLabel)
        
        y += 66
        
        // Platform Diagnostics Card
        let statusBox = FlippedBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 110))
        statusBox.boxType = .custom
        statusBox.cornerRadius = 6
        statusBox.borderWidth = 1
        statusBox.borderColor = .separatorColor
        statusBox.fillColor = NSColor.windowBackgroundColor
        
        var sy: CGFloat = 10
        _ = addInfoRow(label: "Virtualization Engine:", value: "Apple Silicon Hypervisor.framework (ARM64)", in: statusBox, y: sy)
        sy += 24
        let netEntitled = VMManager.shared.hasNetworkingEntitlement() ? "Granted (com.apple.vm.networking)" : "Local Proxy Fallback (vmnet-proxy)"
        _ = addInfoRow(label: "Network Entitlement:", value: netEntitled, in: statusBox, y: sy)
        sy += 24
        _ = addInfoRow(label: "CLI Utilities:", value: "cubectl (manager), cube (vmm)", in: statusBox, y: sy)
        sy += 24
        _ = addInfoRow(label: "Target Guest OS:", value: "FreeBSD ARM64 (RELEASE / CURRENT)", in: statusBox, y: sy)
        
        view.addSubview(statusBox)
        y += 125
        
        let creditsText = NSTextField(wrappingLabelWithString: "Cube is a lightweight native macOS virtual machine manager built specifically for ARM64 guest systems, powered by Hypervisor.framework, virtio, and UEFI.")
        creditsText.font = NSFont.systemFont(ofSize: 11)
        creditsText.textColor = .secondaryLabelColor
        creditsText.frame = NSRect(x: margin, y: y, width: contentWidth, height: 36)
        view.addSubview(creditsText)
        
        // Bottom footer
        let footer = FlippedContainerView(frame: NSRect(x: 0, y: 474, width: 554, height: 46))
        footer.autoresizingMask = [.width, .minYMargin]
        let sep = NSBox(frame: NSRect(x: 0, y: 0, width: 554, height: 1))
        sep.boxType = .separator
        sep.autoresizingMask = [.width]
        footer.addSubview(sep)
        
        let footerNote = NSTextField(labelWithString: "Cube Virtual Machine Manager — \(AppVersion.displayString)")
        footerNote.font = NSFont.systemFont(ofSize: 11)
        footerNote.textColor = .tertiaryLabelColor
        footerNote.frame = NSRect(x: 24, y: 14, width: 506, height: 18)
        footer.addSubview(footerNote)
        
        view.addSubview(footer)
        return view
    }
    
    // MARK: - Footer Builder Helper
    
    private func buildTabFooter(
        width: CGFloat,
        y: CGFloat,
        note: String?,
        cancelTitle: String = "Cancel",
        cancelAction: Selector?,
        applyTitle: String,
        applyAction: Selector
    ) -> NSView {
        let footer = FlippedContainerView(frame: NSRect(x: 0, y: y, width: width, height: 46))
        footer.autoresizingMask = [.width, .minYMargin]
        
        let sep = NSBox(frame: NSRect(x: 0, y: 0, width: width, height: 1))
        sep.boxType = .separator
        sep.autoresizingMask = [.width]
        footer.addSubview(sep)
        
        let applyBtn = NSButton(title: applyTitle, target: self, action: applyAction)
        applyBtn.bezelStyle = .rounded
        let applyWidth: CGFloat = max(110, applyBtn.intrinsicContentSize.width + 24)
        applyBtn.frame = NSRect(x: width - 24 - applyWidth, y: 8, width: applyWidth, height: 30)
        applyBtn.keyEquivalent = "\r"
        footer.addSubview(applyBtn)
        
        var buttonsLeftX = width - 24 - applyWidth
        if let cancelAction = cancelAction {
            let cancelBtn = NSButton(title: cancelTitle, target: self, action: cancelAction)
            cancelBtn.bezelStyle = .rounded
            let cancelWidth: CGFloat = max(75, cancelBtn.intrinsicContentSize.width + 20)
            buttonsLeftX = width - 24 - applyWidth - 10 - cancelWidth
            cancelBtn.frame = NSRect(x: buttonsLeftX, y: 8, width: cancelWidth, height: 30)
            footer.addSubview(cancelBtn)
        }
        
        if let note = note {
            let noteLabel = NSTextField(labelWithString: note)
            noteLabel.font = NSFont.systemFont(ofSize: 11)
            noteLabel.textColor = .secondaryLabelColor
            noteLabel.lineBreakMode = .byTruncatingTail
            let availableWidth = max(50, buttonsLeftX - 24 - 12)
            noteLabel.frame = NSRect(x: 24, y: 14, width: availableWidth, height: 18)
            footer.addSubview(noteLabel)
        }
        
        return footer
    }
    
    // MARK: - Sidebar Selection & Tab Switching
    
    private func switchTab(id: String) {
        currentTabId = id
        for subview in contentContainerView.subviews {
            subview.removeFromSuperview()
        }
        if let tabView = tabViews[id] {
            tabView.frame = contentContainerView.bounds
            tabView.autoresizingMask = [.width, .height]
            contentContainerView.addSubview(tabView)
        }
    }
    
    func numberOfRows(in tableView: NSTableView) -> Int {
        return tabs.count
    }
    
    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard row < tabs.count else { return nil }
        let tab = tabs[row]
        let cellId = NSUserInterfaceItemIdentifier("PrefSidebarCell")
        var cell = tableView.makeView(withIdentifier: cellId, owner: nil)
        if cell == nil {
            let v = NSView(frame: NSRect(x: 0, y: 0, width: 175, height: 34))
            v.identifier = cellId
            
            let iconView = NSImageView(frame: NSRect(x: 10, y: 8, width: 18, height: 18))
            iconView.tag = 201
            v.addSubview(iconView)
            
            let label = NSTextField(labelWithString: "")
            label.tag = 202
            label.font = NSFont.systemFont(ofSize: 13, weight: .medium)
            label.frame = NSRect(x: 34, y: 7, width: 135, height: 20)
            v.addSubview(label)
            
            cell = v
        }
        
        if let iconView = cell?.viewWithTag(201) as? NSImageView {
            let config = NSImage.SymbolConfiguration(pointSize: 14, weight: .regular)
            if let img = NSImage(systemSymbolName: tab.iconName, accessibilityDescription: nil)?.withSymbolConfiguration(config) {
                iconView.image = img
                switch tab.id {
                case "general": iconView.contentTintColor = .systemGray
                case "network": iconView.contentTintColor = .systemBlue
                case "storage": iconView.contentTintColor = .systemPurple
                case "ssh": iconView.contentTintColor = .systemOrange
                case "about": iconView.contentTintColor = .systemTeal
                default: iconView.contentTintColor = .systemBlue
                }
            }
        }
        if let label = cell?.viewWithTag(202) as? NSTextField {
            label.stringValue = tab.title
        }
        return cell
    }
    
    func tableViewSelectionDidChange(_ notification: Notification) {
        let row = sidebarTableView.selectedRow
        guard row >= 0 && row < tabs.count else { return }
        switchTab(id: tabs[row].id)
    }
    
    // MARK: - Row Helper Builders
    
    private func addDiagRow(label: String, value: String, in box: NSBox, y: CGFloat, copyAction: Selector?) -> NSTextField {
        let lbl = NSTextField(labelWithString: label)
        lbl.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        lbl.textColor = .secondaryLabelColor
        lbl.frame = NSRect(x: 12, y: y, width: 120, height: 18)
        box.addSubview(lbl)
        
        let val = NSTextField(labelWithString: value)
        val.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        val.textColor = .labelColor
        val.isSelectable = true
        val.frame = NSRect(x: 135, y: y, width: copyAction != nil ? 280 : 340, height: 18)
        box.addSubview(val)
        
        if let action = copyAction {
            let copyBtn = NSButton(title: "Copy", target: self, action: action)
            copyBtn.bezelStyle = .rounded
            copyBtn.controlSize = .small
            copyBtn.font = NSFont.systemFont(ofSize: 10)
            copyBtn.frame = NSRect(x: 430, y: y - 2, width: 55, height: 20)
            box.addSubview(copyBtn)
        }
        
        return val
    }
    
    private func addInfoRow(label: String, value: String, in box: NSBox, y: CGFloat) -> NSTextField {
        let lbl = NSTextField(labelWithString: label)
        lbl.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        lbl.textColor = .secondaryLabelColor
        lbl.frame = NSRect(x: 12, y: y, width: 175, height: 18)
        box.addSubview(lbl)
        
        let val = NSTextField(labelWithString: value)
        val.font = NSFont.systemFont(ofSize: 11, weight: .regular)
        val.textColor = .labelColor
        val.isSelectable = true
        val.frame = NSRect(x: 190, y: y, width: 295, height: 18)
        box.addSubview(val)
        return val
    }
    
    private func addShortcutRow(action: String, shortcut: String, in box: NSBox, y: CGFloat) -> NSTextField {
        let lbl = NSTextField(labelWithString: action)
        lbl.font = NSFont.systemFont(ofSize: 12, weight: .medium)
        lbl.textColor = .labelColor
        lbl.frame = NSRect(x: 14, y: y, width: 220, height: 18)
        box.addSubview(lbl)
        
        let val = NSTextField(labelWithString: shortcut)
        val.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .semibold)
        val.textColor = .secondaryLabelColor
        val.alignment = .right
        val.frame = NSRect(x: 350, y: y, width: 120, height: 18)
        box.addSubview(val)
        return val
    }
    
    // MARK: - Lifecycle & State
    
    func showTab(_ tabId: String, sender: Any? = nil) {
        AppDelegate.ensureRegularActivation()
        loadCurrentState()
        let targetId = tabViews[tabId] != nil ? tabId : "general"
        currentTabId = targetId
        if let idx = tabs.firstIndex(where: { $0.id == targetId }) {
            sidebarTableView.selectRowIndexes(IndexSet(integer: idx), byExtendingSelection: false)
        }
        switchTab(id: targetId)
        super.showWindow(sender)
        NSApp.activate(ignoringOtherApps: true)
        window?.makeKeyAndOrderFront(sender)
    }

    override func showWindow(_ sender: Any?) {
        showTab(currentTabId, sender: sender)
    }
    
    private func loadCurrentState() {
        loadGeneralState()
        loadNetworkState()
        loadStorageState()
        loadSSHState()
    }
    
    private func loadGeneralState() {
        autoStartCheckbox.state = ServiceManager.shared.autoStartService ? .on : .off
        launchAtLoginCheckbox.state = ServiceManager.shared.isLaunchAtLoginEnabled ? .on : .off
        
        terminalPopUp.removeAllItems()
        let available = VMManager.shared.detectAvailableTerminalApps()
        for app in available {
            terminalPopUp.addItem(withTitle: app.name)
            terminalPopUp.lastItem?.representedObject = app.path
        }
        
        let currentPref = VMManager.shared.preferredTerminalApp
        var found = false
        for item in terminalPopUp.itemArray {
            if let path = item.representedObject as? String,
               (path == currentPref || item.title.lowercased() == currentPref.lowercased()) {
                terminalPopUp.select(item)
                found = true
                break
            }
        }
        if !found && !currentPref.isEmpty && currentPref != "Terminal" {
            let customName = (currentPref as NSString).lastPathComponent.replacingOccurrences(of: ".app", with: "")
            terminalPopUp.insertItem(withTitle: "\(customName) (Custom)", at: 0)
            terminalPopUp.item(at: 0)?.representedObject = currentPref
            terminalPopUp.selectItem(at: 0)
        }
        
        terminalPopUp.menu?.addItem(NSMenuItem.separator())
        let customItem = NSMenuItem(title: "Choose Other...", action: #selector(chooseCustomTerminalClicked), keyEquivalent: "")
        customItem.target = self
        terminalPopUp.menu?.addItem(customItem)
    }
    
    private func loadNetworkState() {
        let hostInfo = detectHostNetworkInfo()
        if let ip4 = hostInfo.ipv4 {
            lastIPv4 = ip4
            hostIPv4Label.stringValue = "\(ip4) (\(hostInfo.interface))"
        } else {
            lastIPv4 = ""
            hostIPv4Label.stringValue = "Not connected"
        }
        
        if let ip6 = hostInfo.ipv6 {
            lastIPv6 = ip6
            hostIPv6Label.stringValue = "\(ip6) (\(hostInfo.interface))"
        } else {
            lastIPv6 = ""
            hostIPv6Label.stringValue = "Not available"
        }
        
        switch ServiceManager.shared.currentMode {
        case .shared(let subnet):
            if let subnet = subnet, !subnet.isEmpty {
                radioAppleDefault.state = .off
                radioCustomSubnet.state = .on
                radioBridged.state = .off
                customSubnetField.stringValue = subnet
                vmGatewayLabel.stringValue = gatewayForSubnet(subnet)
            } else {
                radioAppleDefault.state = .on
                radioCustomSubnet.state = .off
                radioBridged.state = .off
                customSubnetField.stringValue = "192.168.18.0/24"
                vmGatewayLabel.stringValue = "192.168.64.1 (Shared NAT Host Gateway)"
            }
        case .bridged(let iface):
            radioAppleDefault.state = .off
            radioCustomSubnet.state = .off
            radioBridged.state = .on
            if !iface.isEmpty {
                interfacePopUp.selectItem(withTitle: iface)
            }
            vmGatewayLabel.stringValue = "Assigned by physical LAN router"
        }
        updateControlEnablement()
    }
    
    private func loadStorageState() {
        let currentVMsDir = VMManager.shared.vmsDirectory
        vmsDirField.stringValue = currentVMsDir.replacingOccurrences(of: NSHomeDirectory(), with: "~")
        storageFreeSpaceLabel.stringValue = getFreeDiskSpace(at: currentVMsDir)
        storageVMCountLabel.stringValue = "\(VMManager.shared.vms.count) Virtual Machines"
    }
    
    private func loadSSHState() {
        detectedKey = detectSSHKey()
        if let key = detectedKey {
            sshKeyStatusDot.stringValue = "●"
            sshKeyStatusDot.textColor = .systemGreen
            sshKeyPathLabel.stringValue = key.path
            sshKeyTypeLabel.stringValue = "Type: \(key.keyType)"
            sshKeyPreviewField.stringValue = key.keyContent
        } else {
            sshKeyStatusDot.stringValue = "○"
            sshKeyStatusDot.textColor = .secondaryLabelColor
            sshKeyPathLabel.stringValue = "No SSH public key found"
            sshKeyTypeLabel.stringValue = "Type: None (click 'Generate Key' to create one)"
            sshKeyPreviewField.stringValue = "Generate a key or choose an existing .pub file to enable passwordless root SSH login."
        }
    }
    
    private func updateControlEnablement() {
        customSubnetField.isEnabled = (radioCustomSubnet.state == .on)
        interfacePopUp.isEnabled = (radioBridged.state == .on)
    }
    
    private func gatewayForSubnet(_ cidr: String) -> String {
        let parts = cidr.components(separatedBy: "/")
        guard parts.count == 2, let base = parts.first else { return "Unknown" }
        let octets = base.components(separatedBy: ".")
        guard octets.count == 4 else { return "Unknown" }
        return "\(octets[0]).\(octets[1]).\(octets[2]).1 (Shared NAT Gateway)"
    }
    
    private func getFreeDiskSpace(at path: String) -> String {
        let expanded = (path as NSString).expandingTildeInPath
        let url = URL(fileURLWithPath: expanded)
        if let values = try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]),
           let capacity = values.volumeAvailableCapacityForImportantUsage {
            let gib = Double(capacity) / (1024.0 * 1024.0 * 1024.0)
            return String(format: "%.1f GB available", gib)
        }
        return "Available on host drive"
    }
    
    // MARK: - General Tab Actions
    
    @objc private func revertGeneralClicked() {
        loadGeneralState()
    }
    
    @objc private func applyGeneralClicked(_ sender: Any?) {
        ServiceManager.shared.autoStartService = (autoStartCheckbox.state == .on)
        do {
            try ServiceManager.shared.setLaunchAtLogin(enabled: launchAtLoginCheckbox.state == .on)
        } catch {
            print("[Cube] Launch at login registration failed: \(error)")
        }
        
        if let selected = terminalPopUp.selectedItem,
           let path = selected.representedObject as? String {
            VMManager.shared.preferredTerminalApp = path
        } else if let title = terminalPopUp.titleOfSelectedItem {
            VMManager.shared.preferredTerminalApp = title
        }
        
        if let btn = sender as? NSButton {
            let orig = btn.title
            btn.title = "Applied!"
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                btn.title = orig
            }
        }
    }
    
    @objc private func terminalPopUpChanged(_ sender: NSPopUpButton) {
        if sender.selectedItem?.action == #selector(chooseCustomTerminalClicked) {
            chooseCustomTerminalClicked()
        }
    }
    
    @objc private func chooseCustomTerminalClicked() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        panel.message = "Select Preferred Terminal Application (.app)"
        panel.directoryURL = URL(fileURLWithPath: "/Applications")
        if #available(macOS 11.0, *) {
            panel.allowedContentTypes = [.application]
        } else {
            panel.allowedFileTypes = ["app"]
        }
        if panel.runModal() == .OK, let url = panel.url {
            let path = url.path
            let appName = url.deletingPathExtension().lastPathComponent
            let itemTitle = "\(appName) (Custom)"
            
            if let existing = terminalPopUp.item(withTitle: itemTitle) {
                terminalPopUp.select(existing)
            } else {
                terminalPopUp.insertItem(withTitle: itemTitle, at: 0)
                terminalPopUp.item(at: 0)?.representedObject = path
                terminalPopUp.selectItem(at: 0)
            }
        } else {
            loadGeneralState()
        }
    }
    
    // MARK: - Network Tab Actions
    
    @objc private func radioChanged(_ sender: Any?) {
        if sender as? NSButton == radioAppleDefault {
            radioAppleDefault.state = .on
            radioCustomSubnet.state = .off
            radioBridged.state = .off
            vmGatewayLabel.stringValue = "192.168.64.1 (Shared NAT Host Gateway)"
        } else if sender as? NSButton == radioCustomSubnet {
            radioAppleDefault.state = .off
            radioCustomSubnet.state = .on
            radioBridged.state = .off
            let cidr = customSubnetField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            vmGatewayLabel.stringValue = gatewayForSubnet(cidr.isEmpty ? "192.168.18.0/24" : cidr)
        } else if sender as? NSButton == radioBridged {
            radioAppleDefault.state = .off
            radioCustomSubnet.state = .off
            radioBridged.state = .on
            vmGatewayLabel.stringValue = "Assigned by physical LAN router"
        }
        updateControlEnablement()
    }
    
    @objc private func revertNetworkClicked() {
        loadNetworkState()
    }
    
    @objc private func applyNetworkClicked(_ sender: Any?) {
        var newMode: ServiceMode
        
        if radioAppleDefault.state == .on {
            newMode = .shared(subnet: nil)
        } else if radioCustomSubnet.state == .on {
            let cidr = customSubnetField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            if !validateCIDR(cidr) {
                let alert = NSAlert()
                alert.messageText = "Invalid IPv4 CIDR"
                alert.informativeText = "Please enter a valid IPv4 CIDR subnet (e.g. 192.168.18.0/24 or 10.0.0.0/24)."
                alert.alertStyle = .warning
                alert.runModal()
                return
            }
            let alert = NSAlert()
            alert.messageText = "Custom Subnets Require Privileged Setup"
            alert.informativeText = "Select Apple Default for unprivileged networking. Custom subnet configuration is available through the separately administered cube CLI."
            alert.runModal()
            return
        } else {
            let selectedIface = interfacePopUp.titleOfSelectedItem ?? "en0"
            newMode = .bridged(iface: selectedIface)
        }
        
        ServiceManager.shared.saveMode(newMode)
        
        let applyBtn = sender as? NSButton
        let origTitle = applyBtn?.title ?? "Apply & Restart"
        applyBtn?.title = "Restarting..."
        applyBtn?.isEnabled = false
        
        let completion: (Bool, String?) -> Void = { [weak applyBtn] success, error in
            DispatchQueue.main.async {
                applyBtn?.isEnabled = true
                applyBtn?.title = origTitle
                if !success, let error = error {
                    let alert = NSAlert()
                    alert.messageText = "Network Service Could Not Apply Settings"
                    alert.informativeText = error
                    alert.runModal()
                } else {
                    applyBtn?.title = "Applied!"
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                        applyBtn?.title = origTitle
                    }
                }
            }
        }
        if ServiceManager.shared.isRunning {
            ServiceManager.shared.restartService(completion: completion)
        } else if ServiceManager.shared.autoStartService {
            ServiceManager.shared.startService(completion: completion)
        } else {
            completion(true, nil)
        }
    }
    
    @objc private func copyIPv4Clicked() {
        guard !lastIPv4.isEmpty else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(lastIPv4, forType: .string)
    }
    
    @objc private func copyIPv6Clicked() {
        guard !lastIPv6.isEmpty else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(lastIPv6, forType: .string)
    }
    
    @objc private func copySocketClicked() {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(ServiceManager.shared.defaultSocketPath, forType: .string)
    }
    
    // MARK: - Storage Tab Actions
    
    @objc private func revertStorageClicked() {
        loadStorageState()
    }
    
    @objc private func applyStorageClicked(_ sender: Any?) {
        let inputVMsDir = vmsDirField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if !inputVMsDir.isEmpty {
            let expanded = (inputVMsDir as NSString).expandingTildeInPath
            try? FileManager.default.createDirectory(atPath: expanded, withIntermediateDirectories: true)
            VMManager.shared.vmsDirectory = expanded
            VMManager.shared.refreshVMs()
            storageFreeSpaceLabel.stringValue = getFreeDiskSpace(at: expanded)
            storageVMCountLabel.stringValue = "\(VMManager.shared.vms.count) Virtual Machines"
        }
        if let btn = sender as? NSButton {
            let orig = btn.title
            btn.title = "Applied!"
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                btn.title = orig
            }
        }
    }
    
    @objc private func chooseVMsDirClicked() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Select"
        panel.message = "Choose Default Virtual Machine Storage Directory"
        let currentPath = (vmsDirField.stringValue as NSString).expandingTildeInPath
        if FileManager.default.fileExists(atPath: currentPath) {
            panel.directoryURL = URL(fileURLWithPath: currentPath)
        }
        if panel.runModal() == .OK, let url = panel.url {
            vmsDirField.stringValue = url.path.replacingOccurrences(of: NSHomeDirectory(), with: "~")
            storageFreeSpaceLabel.stringValue = getFreeDiskSpace(at: url.path)
        }
    }

    @objc private func revealVMsDirClicked() {
        let path = (vmsDirField.stringValue as NSString).expandingTildeInPath
        if !FileManager.default.fileExists(atPath: path) {
            try? FileManager.default.createDirectory(atPath: path, withIntermediateDirectories: true)
        }
        NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: path)
    }

    @objc private func resetVMsDirClicked() {
        vmsDirField.stringValue = "~/Cube"
        storageFreeSpaceLabel.stringValue = getFreeDiskSpace(at: "~/Cube")
    }

    // MARK: - SSH Key Management Actions
    
    private func detectSSHKey() -> DetectedSSHKey? {
        let home = NSHomeDirectory()
        let settingsUrl = URL(fileURLWithPath: "\(home)/.local/state/cube/settings.json")
        if let data = try? Data(contentsOf: settingsUrl),
           let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let customPath = json["ssh_key_path"] as? String, !customPath.isEmpty {
            let expanded = (customPath as NSString).expandingTildeInPath
            if let content = try? String(contentsOfFile: expanded, encoding: .utf8), !content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                return parseKey(path: customPath, content: content.trimmingCharacters(in: .whitespacesAndNewlines))
            }
        }
        
        let candidates = ["id_ed25519.pub", "id_rsa.pub", "id_ecdsa.pub"]
        for cand in candidates {
            let fullPath = "\(home)/.ssh/\(cand)"
            if let content = try? String(contentsOfFile: fullPath, encoding: .utf8), !content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                return parseKey(path: "~/.ssh/\(cand)", content: content.trimmingCharacters(in: .whitespacesAndNewlines))
            }
        }
        return nil
    }

    private func parseKey(path: String, content: String) -> DetectedSSHKey {
        let parts = content.components(separatedBy: " ")
        var keyType = "SSH Key"
        if let first = parts.first {
            if first.contains("ed25519") { keyType = "ED25519 (256-bit elliptic curve)" }
            else if first.contains("rsa") { keyType = "RSA Public Key" }
            else if first.contains("ecdsa") { keyType = "ECDSA Public Key" }
        }
        var fp = ""
        if parts.count >= 2 {
            let b64 = parts[1]
            let prefix = String(b64.prefix(12))
            let suffix = String(b64.suffix(12))
            fp = "\(prefix)...\(suffix)"
        }
        return DetectedSSHKey(path: path, keyType: keyType, keyContent: content, fingerprint: fp)
    }

    @objc private func copySSHKeyClicked(_ sender: Any?) {
        guard let key = detectedKey else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(key.keyContent, forType: .string)
        if let btn = sender as? NSButton {
            btn.title = "Copied!"
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                btn.title = "Copy Public Key"
            }
        }
    }

    @objc private func revealSSHKeyClicked() {
        guard let key = detectedKey else {
            let home = NSHomeDirectory()
            let sshDir = "\(home)/.ssh"
            try? FileManager.default.createDirectory(atPath: sshDir, withIntermediateDirectories: true)
            NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: sshDir)
            return
        }
        let expanded = (key.path as NSString).expandingTildeInPath
        NSWorkspace.shared.selectFile(expanded, inFileViewerRootedAtPath: (expanded as NSString).deletingLastPathComponent)
    }

    @objc private func chooseSSHKeyClicked() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Select Key"
        panel.message = "Choose an SSH Public Key (.pub)"
        panel.directoryURL = URL(fileURLWithPath: "\(NSHomeDirectory())/.ssh")
        if panel.runModal() == .OK, let url = panel.url {
            let path = url.path.replacingOccurrences(of: NSHomeDirectory(), with: "~")
            saveCustomSSHKeyPath(path)
            loadCurrentState()
        }
    }

    @objc private func generateSSHKeyClicked() {
        let home = NSHomeDirectory()
        let sshDir = "\(home)/.ssh"
        let keyPath = "\(sshDir)/id_ed25519"
        if FileManager.default.fileExists(atPath: "\(keyPath).pub") {
            let alert = NSAlert()
            alert.messageText = "SSH Key Already Exists"
            alert.informativeText = "A key already exists at ~/.ssh/id_ed25519.pub."
            alert.alertStyle = .informational
            alert.runModal()
            return
        }
        try? FileManager.default.createDirectory(atPath: sshDir, withIntermediateDirectories: true)
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/ssh-keygen")
        proc.arguments = ["-t", "ed25519", "-f", keyPath, "-N", "", "-C", "cube-vm-admin"]
        do {
            try proc.run()
            proc.waitUntilExit()
            loadCurrentState()
        } catch {
            let alert = NSAlert()
            alert.messageText = "Key Generation Failed"
            alert.informativeText = error.localizedDescription
            alert.alertStyle = .warning
            alert.runModal()
        }
    }

    private func saveCustomSSHKeyPath(_ path: String) {
        let home = NSHomeDirectory()
        let stateDir = URL(fileURLWithPath: "\(home)/.local/state/cube")
        try? FileManager.default.createDirectory(at: stateDir, withIntermediateDirectories: true)
        let settingsUrl = stateDir.appendingPathComponent("settings.json")
        var json: [String: Any] = [:]
        if let data = try? Data(contentsOf: settingsUrl),
           let existing = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            json = existing
        }
        let trimmed = path.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed == "~/.ssh/id_ed25519.pub" || trimmed == "\(home)/.ssh/id_ed25519.pub" {
            json.removeValue(forKey: "ssh_key_path")
        } else {
            json["ssh_key_path"] = (trimmed as NSString).expandingTildeInPath
        }
        if let data = try? JSONSerialization.data(withJSONObject: json, options: [.prettyPrinted]) {
            try? data.write(to: settingsUrl)
        }
    }

    private func validateCIDR(_ cidr: String) -> Bool {
        let parts = cidr.components(separatedBy: "/")
        guard parts.count == 2,
              let prefix = Int(parts[1]),
              prefix >= 8 && prefix <= 30 else {
            return false
        }
        let octets = parts[0].components(separatedBy: ".")
        guard octets.count == 4 else { return false }
        for octet in octets {
            guard let val = Int(octet), val >= 0 && val <= 255 else { return false }
        }
        return true
    }
    
    private func detectInterfaces() -> [String] {
        var names: [String] = []
        var ifap: UnsafeMutablePointer<ifaddrs>?
        if getifaddrs(&ifap) == 0, let first = ifap {
            defer { freeifaddrs(ifap) }
            var cursor: UnsafeMutablePointer<ifaddrs>? = first
            while let ptr = cursor {
                let name = String(cString: ptr.pointee.ifa_name)
                if name.starts(with: "en") && !names.contains(name) {
                    names.append(name)
                }
                cursor = ptr.pointee.ifa_next
            }
        }
        return names.isEmpty ? ["en0"] : names.sorted()
    }
}
