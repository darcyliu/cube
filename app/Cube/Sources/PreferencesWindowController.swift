import Foundation
import AppKit
import ServiceManagement
import UniformTypeIdentifiers

private class FlippedContainerView: NSView {
    override var isFlipped: Bool { return true }
}

private class FlippedClipView: NSClipView {
    override var isFlipped: Bool { return true }
}

private class CardView: NSView {
    init() {
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        wantsLayer = true
        layer?.cornerRadius = CubeTokens.cardCornerRadius
        layer?.borderWidth = CubeTokens.cardBorderWidth
        layer?.borderColor = CubeTokens.cardBorder.cgColor
        layer?.backgroundColor = CubeTokens.cardBackground.cgColor
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func updateColors() {
        layer?.borderColor = CubeTokens.cardBorder.cgColor
        layer?.backgroundColor = CubeTokens.cardBackground.cgColor
    }
}

private class ActionButton: NSButton {
    var onClick: ((ActionButton) -> Void)?

    init(title: String) {
        super.init(frame: .zero)
        self.translatesAutoresizingMaskIntoConstraints = false
        self.title = title
        self.target = self
        self.action = #selector(internalAction)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    @objc private func internalAction() {
        onClick?(self)
    }
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
        PreferenceTabItem(id: "images", title: "Images", iconName: "opticaldisc"),
        PreferenceTabItem(id: "ssh", title: "SSH Keys", iconName: "key"),
        PreferenceTabItem(id: "about", title: "About", iconName: "info.circle")
    ]
    
    private var sidebarTableView: NSTableView!
    private var contentContainerView: NSView!
    private var tabViews: [String: NSView] = [:]
    private var currentTabId: String = "general"
    
    private var rootVisualEffect: NSVisualEffectView!
    private var sidebarContainer: NSVisualEffectView!

    deinit {
        NSWorkspace.shared.notificationCenter.removeObserver(self)
    }

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
    
    // Images Tab Controls
    private var imagesTotalUsageLabel: NSTextField!
    private var imagesListContainer: NSStackView!
    private var imagesScrollView: NSScrollView!

    // SSH Keys Tab Controls
    private var sshKeyStatusDot: NSTextField!
    private var sshKeyPathLabel: NSTextField!
    private var sshKeyTypeLabel: NSTextField!
    private var sshKeyPreviewField: NSTextField!
    private var detectedKey: DetectedSSHKey?

    convenience init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 750, height: 540),
            styleMask: [.titled, .closable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Cube Preferences — \(AppVersion.displayString)"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isOpaque = false
        window.backgroundColor = .clear
        window.minSize = NSSize(width: 750, height: 540)
        window.center()
        window.isReleasedWhenClosed = false
        
        self.init(window: window)
        setupUI()
    }

    private func setupUI() {
        guard let window = self.window else { return }
        
        rootVisualEffect = NSVisualEffectView(frame: window.contentView!.bounds)
        rootVisualEffect.material = .underWindowBackground
        rootVisualEffect.blendingMode = .behindWindow
        rootVisualEffect.state = .active
        rootVisualEffect.autoresizingMask = [.width, .height]
        rootVisualEffect.wantsLayer = true
        rootVisualEffect.layer?.cornerRadius = 18.0
        rootVisualEffect.layer?.masksToBounds = true
        rootVisualEffect.layer?.borderWidth = 1.0
        rootVisualEffect.layer?.borderColor = CubeTokens.windowBorder.cgColor
        window.contentView = rootVisualEffect
        
        let sidebarWidth: CGFloat = 180
        
        // 1. Left Sidebar View (Translucent frosted glass sidebar)
        sidebarContainer = NSVisualEffectView()
        sidebarContainer.translatesAutoresizingMaskIntoConstraints = false
        sidebarContainer.material = .sidebar
        sidebarContainer.blendingMode = .behindWindow
        sidebarContainer.state = .active
        rootVisualEffect.addSubview(sidebarContainer)

        let headerHeight: CGFloat = 80

        // Top Header - 80pt height to cleanly clear native window traffic light buttons at top-left
        let header = NSView()
        header.translatesAutoresizingMaskIntoConstraints = false

        let titleLabel = NSTextField(labelWithString: "PREFERENCES")
        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.font = CubeTokens.sectionHeaderFont
        titleLabel.textColor = CubeTokens.secondaryText
        header.addSubview(titleLabel)

        let sepTop = NSBox()
        sepTop.translatesAutoresizingMaskIntoConstraints = false
        sepTop.boxType = .custom
        sepTop.borderWidth = 0
        sepTop.fillColor = CubeTokens.separatorColor
        header.addSubview(sepTop)
        sidebarContainer.addSubview(header)
        
        let sidebarScrollView = NSScrollView()
        sidebarScrollView.translatesAutoresizingMaskIntoConstraints = false
        sidebarScrollView.hasVerticalScroller = false
        sidebarScrollView.borderType = .noBorder
        sidebarScrollView.drawsBackground = false
        
        sidebarTableView = NSTableView()
        sidebarTableView.headerView = nil
        sidebarTableView.rowHeight = 36
        if #available(macOS 11.0, *) {
            sidebarTableView.style = .sourceList
        } else {
            sidebarTableView.selectionHighlightStyle = .sourceList
        }
        sidebarTableView.backgroundColor = .clear
        
        let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("PrefTabCol"))
        col.width = sidebarWidth
        sidebarTableView.addTableColumn(col)
        
        sidebarTableView.dataSource = self
        sidebarTableView.delegate = self
        sidebarScrollView.documentView = sidebarTableView
        sidebarContainer.addSubview(sidebarScrollView)
        
        // 2. Vertical Divider line
        let divider = NSBox()
        divider.translatesAutoresizingMaskIntoConstraints = false
        divider.boxType = .custom
        divider.borderWidth = 0
        divider.fillColor = CubeTokens.separatorColor
        rootVisualEffect.addSubview(divider)
        
        // 3. Right Content Container (Full Height, per-tab footers)
        contentContainerView = NSView()
        contentContainerView.translatesAutoresizingMaskIntoConstraints = false
        contentContainerView.wantsLayer = true
        contentContainerView.layer?.backgroundColor = NSColor.clear.cgColor
        rootVisualEffect.addSubview(contentContainerView)
        
        NSLayoutConstraint.activate([
            // Sidebar constraints
            sidebarContainer.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            sidebarContainer.topAnchor.constraint(equalTo: rootVisualEffect.topAnchor),
            sidebarContainer.bottomAnchor.constraint(equalTo: rootVisualEffect.bottomAnchor),
            sidebarContainer.widthAnchor.constraint(equalToConstant: sidebarWidth),
            
            // Header inside sidebar
            header.topAnchor.constraint(equalTo: sidebarContainer.topAnchor),
            header.leadingAnchor.constraint(equalTo: sidebarContainer.leadingAnchor),
            header.trailingAnchor.constraint(equalTo: sidebarContainer.trailingAnchor),
            header.heightAnchor.constraint(equalToConstant: headerHeight),
            
            titleLabel.leadingAnchor.constraint(equalTo: header.leadingAnchor, constant: 18),
            titleLabel.trailingAnchor.constraint(equalTo: header.trailingAnchor, constant: -18),
            titleLabel.bottomAnchor.constraint(equalTo: header.bottomAnchor, constant: -14),
            
            sepTop.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            sepTop.trailingAnchor.constraint(equalTo: header.trailingAnchor),
            sepTop.bottomAnchor.constraint(equalTo: header.bottomAnchor),
            sepTop.heightAnchor.constraint(equalToConstant: 1),
            
            // Sidebar scroll view
            sidebarScrollView.topAnchor.constraint(equalTo: header.bottomAnchor),
            sidebarScrollView.leadingAnchor.constraint(equalTo: sidebarContainer.leadingAnchor),
            sidebarScrollView.trailingAnchor.constraint(equalTo: sidebarContainer.trailingAnchor),
            sidebarScrollView.bottomAnchor.constraint(equalTo: sidebarContainer.bottomAnchor),
            
            // Divider
            divider.leadingAnchor.constraint(equalTo: sidebarContainer.trailingAnchor),
            divider.topAnchor.constraint(equalTo: rootVisualEffect.topAnchor),
            divider.bottomAnchor.constraint(equalTo: rootVisualEffect.bottomAnchor),
            divider.widthAnchor.constraint(equalToConstant: 1),
            
            // Content Container
            contentContainerView.leadingAnchor.constraint(equalTo: divider.trailingAnchor),
            contentContainerView.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            contentContainerView.topAnchor.constraint(equalTo: rootVisualEffect.topAnchor),
            contentContainerView.bottomAnchor.constraint(equalTo: rootVisualEffect.bottomAnchor)
        ])
        
        // 4. Pre-build all Tab Views
        tabViews["general"] = buildGeneralTabView()
        tabViews["network"] = buildNetworkTabView()
        tabViews["storage"] = buildStorageTabView()
        tabViews["images"] = buildImagesTabView()
        tabViews["ssh"] = buildSSHKeysTabView()
        tabViews["about"] = buildAboutTabView()

        // Hook up background image cache updates
        ImageCacheManager.shared.onProgressChanged = { [weak self] _, _ in
            if self?.currentTabId == "images" {
                self?.refreshImagesUI()
            }
        }
        ImageCacheManager.shared.onCacheUpdated = { [weak self] in
            if self?.currentTabId == "images" {
                self?.refreshImagesUI()
            }
        }
        ImageCacheManager.shared.onDownloadFinished = { [weak self] presetId, success, errorMsg in
            if !success, let msg = errorMsg {
                let alert = NSAlert()
                alert.messageText = "Download Failed"
                alert.informativeText = "Could not download image (\(presetId)):\n\(msg)"
                alert.alertStyle = .warning
                alert.runModal()
            }
            if self?.currentTabId == "images" {
                self?.refreshImagesUI()
            }
        }
        
        // Select initial tab: General
        sidebarTableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        switchTab(id: "general")

        // Accessibility Listeners
        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(accessibilityDisplayOptionsDidChange),
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification,
            object: nil
        )
        updateAccessibilityPreferences()
    }

    private func createCardBox() -> CardView {
        return CardView()
    }
    
    // MARK: - Tab Views Builders
    
    private func buildGeneralTabView() -> NSView {
        let view = NSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        
        let header = NSTextField(labelWithString: "General & Automation")
        header.translatesAutoresizingMaskIntoConstraints = false
        header.font = NSFont.boldSystemFont(ofSize: 15)
        header.textColor = CubeTokens.primaryText
        view.addSubview(header)
        
        // Startup options card
        let startupBox = createCardBox()
        view.addSubview(startupBox)
        
        autoStartCheckbox = NSButton(checkboxWithTitle: "Automatically start Cube network service on launch", target: nil, action: nil)
        autoStartCheckbox.translatesAutoresizingMaskIntoConstraints = false
        autoStartCheckbox.font = NSFont.systemFont(ofSize: 13)
        startupBox.addSubview(autoStartCheckbox)
        
        launchAtLoginCheckbox = NSButton(checkboxWithTitle: "Launch Cube menu bar application at macOS login", target: nil, action: nil)
        launchAtLoginCheckbox.translatesAutoresizingMaskIntoConstraints = false
        launchAtLoginCheckbox.font = NSFont.systemFont(ofSize: 13)
        startupBox.addSubview(launchAtLoginCheckbox)
        
        // Preferred Terminal Card
        let termBox = createCardBox()
        view.addSubview(termBox)
        
        let termLabel = NSTextField(labelWithString: "Preferred Terminal Application:")
        termLabel.translatesAutoresizingMaskIntoConstraints = false
        termLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        termLabel.textColor = CubeTokens.secondaryText
        termBox.addSubview(termLabel)
        
        terminalPopUp = NSPopUpButton(frame: .zero, pullsDown: false)
        terminalPopUp.translatesAutoresizingMaskIntoConstraints = false
        terminalPopUp.target = self
        terminalPopUp.action = #selector(terminalPopUpChanged(_:))
        termBox.addSubview(terminalPopUp)
        
        let termHint = NSTextField(wrappingLabelWithString: "Used when opening SSH terminal sessions from the dashboard and menu bar.")
        termHint.translatesAutoresizingMaskIntoConstraints = false
        termHint.font = NSFont.systemFont(ofSize: 11)
        termHint.textColor = CubeTokens.tertiaryText
        termBox.addSubview(termHint)
        
        // Keyboard Shortcuts header
        let shortcutsHeader = NSTextField(labelWithString: "Global Keyboard Shortcuts")
        shortcutsHeader.translatesAutoresizingMaskIntoConstraints = false
        shortcutsHeader.font = NSFont.boldSystemFont(ofSize: 13)
        shortcutsHeader.textColor = CubeTokens.primaryText
        view.addSubview(shortcutsHeader)
        
        let scBox = createCardBox()
        view.addSubview(scBox)
        
        let (_, r1Bottom) = addShortcutRow(action: "Open Dashboard:", shortcut: "⌘ D", in: scBox, topAnchor: scBox.topAnchor, topOffset: 12)
        let (_, r2Bottom) = addShortcutRow(action: "New Virtual Machine:", shortcut: "⌘ N", in: scBox, topAnchor: r1Bottom, topOffset: 8)
        let (_, r3Bottom) = addShortcutRow(action: "View Virtual Machine Logs:", shortcut: "⌘ L", in: scBox, topAnchor: r2Bottom, topOffset: 8)
        let (_, r4Bottom) = addShortcutRow(action: "Open Preferences:", shortcut: "⌘ ,", in: scBox, topAnchor: r3Bottom, topOffset: 8)
        
        // Per-tab action footer
        let footer = buildTabFooter(
            note: "Applies to current user session.",
            cancelTitle: "Cancel",
            cancelAction: #selector(revertGeneralClicked),
            applyTitle: "Apply",
            applyAction: #selector(applyGeneralClicked(_:))
        )
        view.addSubview(footer)
        
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: view.topAnchor, constant: 26),
            header.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            header.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            // Startup Card
            startupBox.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 12),
            startupBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            startupBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            autoStartCheckbox.topAnchor.constraint(equalTo: startupBox.topAnchor, constant: 14),
            autoStartCheckbox.leadingAnchor.constraint(equalTo: startupBox.leadingAnchor, constant: 14),
            autoStartCheckbox.trailingAnchor.constraint(lessThanOrEqualTo: startupBox.trailingAnchor, constant: -14),
            
            launchAtLoginCheckbox.topAnchor.constraint(equalTo: autoStartCheckbox.bottomAnchor, constant: 10),
            launchAtLoginCheckbox.leadingAnchor.constraint(equalTo: startupBox.leadingAnchor, constant: 14),
            launchAtLoginCheckbox.trailingAnchor.constraint(lessThanOrEqualTo: startupBox.trailingAnchor, constant: -14),
            launchAtLoginCheckbox.bottomAnchor.constraint(equalTo: startupBox.bottomAnchor, constant: -14),
            
            // Terminal Card
            termBox.topAnchor.constraint(equalTo: startupBox.bottomAnchor, constant: 14),
            termBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            termBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            termLabel.topAnchor.constraint(equalTo: termBox.topAnchor, constant: 12),
            termLabel.leadingAnchor.constraint(equalTo: termBox.leadingAnchor, constant: 14),
            termLabel.trailingAnchor.constraint(lessThanOrEqualTo: termBox.trailingAnchor, constant: -14),
            
            terminalPopUp.topAnchor.constraint(equalTo: termLabel.bottomAnchor, constant: 8),
            terminalPopUp.leadingAnchor.constraint(equalTo: termBox.leadingAnchor, constant: 14),
            terminalPopUp.widthAnchor.constraint(equalToConstant: 240),
            terminalPopUp.bottomAnchor.constraint(equalTo: termBox.bottomAnchor, constant: -14),
            
            termHint.leadingAnchor.constraint(equalTo: terminalPopUp.trailingAnchor, constant: 14),
            termHint.trailingAnchor.constraint(equalTo: termBox.trailingAnchor, constant: -14),
            termHint.centerYAnchor.constraint(equalTo: terminalPopUp.centerYAnchor),
            
            // Shortcuts Header & Card
            shortcutsHeader.topAnchor.constraint(equalTo: termBox.bottomAnchor, constant: 18),
            shortcutsHeader.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            shortcutsHeader.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            scBox.topAnchor.constraint(equalTo: shortcutsHeader.bottomAnchor, constant: 10),
            scBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            scBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            scBox.bottomAnchor.constraint(equalTo: r4Bottom, constant: 12),
            
            // Footer
            footer.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            footer.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            footer.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            footer.heightAnchor.constraint(equalToConstant: 48)
        ])
        
        return view
    }
    
    private func buildNetworkTabView() -> NSView {
        let view = NSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        
        let header = NSTextField(labelWithString: "Network Operation Mode")
        header.translatesAutoresizingMaskIntoConstraints = false
        header.font = NSFont.boldSystemFont(ofSize: 15)
        header.textColor = CubeTokens.primaryText
        view.addSubview(header)
        
        // Radio 1: Apple Default NAT
        radioAppleDefault = NSButton(radioButtonWithTitle: "Shared NAT — Apple Default (Recommended)", target: self, action: #selector(radioChanged))
        radioAppleDefault.translatesAutoresizingMaskIntoConstraints = false
        radioAppleDefault.font = NSFont.systemFont(ofSize: 13, weight: .medium)
        view.addSubview(radioAppleDefault)
        
        let appleDefaultHint = NSTextField(wrappingLabelWithString: "Uses Apple's native subnet (192.168.64.0/24). Runs 100% unprivileged without root or sudo.")
        appleDefaultHint.translatesAutoresizingMaskIntoConstraints = false
        appleDefaultHint.font = NSFont.systemFont(ofSize: 11)
        appleDefaultHint.textColor = CubeTokens.secondaryText
        view.addSubview(appleDefaultHint)
        
        // Radio 2: Custom Subnet NAT
        radioCustomSubnet = NSButton(radioButtonWithTitle: "Shared NAT — Custom Subnet", target: self, action: #selector(radioChanged))
        radioCustomSubnet.translatesAutoresizingMaskIntoConstraints = false
        radioCustomSubnet.font = NSFont.systemFont(ofSize: 13)
        view.addSubview(radioCustomSubnet)
        
        customSubnetField = NSTextField()
        customSubnetField.translatesAutoresizingMaskIntoConstraints = false
        customSubnetField.placeholderString = "192.168.18.0/24"
        customSubnetField.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        view.addSubview(customSubnetField)
        
        let customSubnetHint = NSTextField(wrappingLabelWithString: "Specify an RFC1918 CIDR. Requires a separately administered root service; unavailable in Cube.")
        customSubnetHint.translatesAutoresizingMaskIntoConstraints = false
        customSubnetHint.font = NSFont.systemFont(ofSize: 11)
        customSubnetHint.textColor = CubeTokens.secondaryText
        view.addSubview(customSubnetHint)
        
        // Radio 3: Bridged LAN
        radioBridged = NSButton(radioButtonWithTitle: "Bridged LAN (Physical Interface)", target: self, action: #selector(radioChanged))
        radioBridged.translatesAutoresizingMaskIntoConstraints = false
        radioBridged.font = NSFont.systemFont(ofSize: 13)
        view.addSubview(radioBridged)
        
        interfacePopUp = NSPopUpButton(frame: .zero, pullsDown: false)
        interfacePopUp.translatesAutoresizingMaskIntoConstraints = false
        for iface in detectInterfaces() {
            interfacePopUp.addItem(withTitle: iface)
        }
        view.addSubview(interfacePopUp)
        
        let bridgedHint = NSTextField(wrappingLabelWithString: "Attaches guests to the physical network for external DHCP address assignment.")
        bridgedHint.translatesAutoresizingMaskIntoConstraints = false
        bridgedHint.font = NSFont.systemFont(ofSize: 11)
        bridgedHint.textColor = CubeTokens.secondaryText
        view.addSubview(bridgedHint)
        
        // Card: Diagnostics & Host Information (Compresto Frosted Glass)
        let diagBox = createCardBox()
        view.addSubview(diagBox)
        
        let (v1, d1Bottom) = addDiagRow(label: "Host IPv4 (en0):", value: "Detecting...", in: diagBox, topAnchor: diagBox.topAnchor, topOffset: 12, copyAction: #selector(copyIPv4Clicked(_:)))
        hostIPv4Label = v1
        let (v2, d2Bottom) = addDiagRow(label: "Host IPv6:", value: "Detecting...", in: diagBox, topAnchor: d1Bottom, topOffset: 8, copyAction: #selector(copyIPv6Clicked(_:)))
        hostIPv6Label = v2
        let (v3, d3Bottom) = addDiagRow(label: "VM Gateway:", value: "192.168.64.1", in: diagBox, topAnchor: d2Bottom, topOffset: 8, copyAction: nil)
        vmGatewayLabel = v3
        let (v4, d4Bottom) = addDiagRow(label: "Daemon Socket:", value: ServiceManager.shared.defaultSocketPath, in: diagBox, topAnchor: d3Bottom, topOffset: 8, copyAction: #selector(copySocketClicked(_:)))
        socketLabel = v4
        
        // Per-tab action footer
        let footer = buildTabFooter(
            note: "Requires service restart.",
            cancelTitle: "Cancel",
            cancelAction: #selector(revertNetworkClicked),
            applyTitle: "Apply & Restart",
            applyAction: #selector(applyNetworkClicked(_:))
        )
        view.addSubview(footer)
        
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: view.topAnchor, constant: 26),
            header.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            header.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            // Radio 1
            radioAppleDefault.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 14),
            radioAppleDefault.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            radioAppleDefault.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            appleDefaultHint.topAnchor.constraint(equalTo: radioAppleDefault.bottomAnchor, constant: 4),
            appleDefaultHint.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 44),
            appleDefaultHint.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            // Radio 2
            radioCustomSubnet.topAnchor.constraint(equalTo: appleDefaultHint.bottomAnchor, constant: 12),
            radioCustomSubnet.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            
            customSubnetField.leadingAnchor.constraint(equalTo: radioCustomSubnet.trailingAnchor, constant: 8),
            customSubnetField.centerYAnchor.constraint(equalTo: radioCustomSubnet.centerYAnchor),
            customSubnetField.widthAnchor.constraint(equalToConstant: 170),
            customSubnetField.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            customSubnetHint.topAnchor.constraint(equalTo: radioCustomSubnet.bottomAnchor, constant: 4),
            customSubnetHint.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 44),
            customSubnetHint.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            // Radio 3
            radioBridged.topAnchor.constraint(equalTo: customSubnetHint.bottomAnchor, constant: 12),
            radioBridged.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            
            interfacePopUp.leadingAnchor.constraint(equalTo: radioBridged.trailingAnchor, constant: 8),
            interfacePopUp.centerYAnchor.constraint(equalTo: radioBridged.centerYAnchor),
            interfacePopUp.widthAnchor.constraint(equalToConstant: 150),
            interfacePopUp.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            bridgedHint.topAnchor.constraint(equalTo: radioBridged.bottomAnchor, constant: 4),
            bridgedHint.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 44),
            bridgedHint.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            // Diagnostics Card
            diagBox.topAnchor.constraint(equalTo: bridgedHint.bottomAnchor, constant: 16),
            diagBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            diagBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            diagBox.bottomAnchor.constraint(equalTo: d4Bottom, constant: 12),
            
            // Footer
            footer.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            footer.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            footer.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            footer.heightAnchor.constraint(equalToConstant: 48)
        ])
        
        return view
    }
    
    private func buildStorageTabView() -> NSView {
        let view = NSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        
        let header = NSTextField(labelWithString: "Virtual Machine Storage")
        header.translatesAutoresizingMaskIntoConstraints = false
        header.font = NSFont.boldSystemFont(ofSize: 15)
        header.textColor = CubeTokens.primaryText
        view.addSubview(header)
        
        let desc = NSTextField(wrappingLabelWithString: "Select the primary folder for storing virtual disk images (rootfs.img), hardware configs, and VM state directories. External SSDs (Thunderbolt / USB4) are fully supported.")
        desc.translatesAutoresizingMaskIntoConstraints = false
        desc.font = NSFont.systemFont(ofSize: 12)
        desc.textColor = CubeTokens.secondaryText
        view.addSubview(desc)
        
        // Storage path card
        let pathBox = createCardBox()
        view.addSubview(pathBox)
        
        let pathLabel = NSTextField(labelWithString: "Storage Directory:")
        pathLabel.translatesAutoresizingMaskIntoConstraints = false
        pathLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        pathLabel.textColor = CubeTokens.secondaryText
        pathBox.addSubview(pathLabel)
        
        vmsDirField = NSTextField()
        vmsDirField.translatesAutoresizingMaskIntoConstraints = false
        vmsDirField.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        vmsDirField.placeholderString = "~/Cube"
        pathBox.addSubview(vmsDirField)
        
        let chooseBtn = NSButton(title: "Choose...", target: self, action: #selector(chooseVMsDirClicked))
        chooseBtn.translatesAutoresizingMaskIntoConstraints = false
        chooseBtn.bezelStyle = .rounded
        pathBox.addSubview(chooseBtn)
        
        let revealBtn = NSButton(title: "Reveal", target: self, action: #selector(revealVMsDirClicked))
        revealBtn.translatesAutoresizingMaskIntoConstraints = false
        revealBtn.bezelStyle = .rounded
        pathBox.addSubview(revealBtn)
        
        let resetBtn = NSButton(title: "Reset", target: self, action: #selector(resetVMsDirClicked))
        resetBtn.translatesAutoresizingMaskIntoConstraints = false
        resetBtn.bezelStyle = .rounded
        pathBox.addSubview(resetBtn)
        
        // Storage Stats card
        let statsBox = createCardBox()
        view.addSubview(statsBox)
        
        let (s1, st1Bottom) = addInfoRow(label: "Volume Free Space:", value: "Calculating...", in: statsBox, topAnchor: statsBox.topAnchor, topOffset: 12)
        storageFreeSpaceLabel = s1
        let (s2, st2Bottom) = addInfoRow(label: "Configured Virtual Machines:", value: "0 VMs", in: statsBox, topAnchor: st1Bottom, topOffset: 8)
        storageVMCountLabel = s2
        let (_, st3Bottom) = addInfoRow(label: "Image Format:", value: "Raw Sparse Disk Image (*.raw / *.img)", in: statsBox, topAnchor: st2Bottom, topOffset: 8)
        
        // Per-tab action footer
        let footer = buildTabFooter(
            note: "VMs are loaded dynamically from this location.",
            cancelTitle: "Cancel",
            cancelAction: #selector(revertStorageClicked),
            applyTitle: "Apply",
            applyAction: #selector(applyStorageClicked(_:))
        )
        view.addSubview(footer)
        
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: view.topAnchor, constant: 26),
            header.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            header.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            desc.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 6),
            desc.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            desc.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            // Path Box
            pathBox.topAnchor.constraint(equalTo: desc.bottomAnchor, constant: 14),
            pathBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            pathBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            pathLabel.topAnchor.constraint(equalTo: pathBox.topAnchor, constant: 12),
            pathLabel.leadingAnchor.constraint(equalTo: pathBox.leadingAnchor, constant: 14),
            pathLabel.trailingAnchor.constraint(lessThanOrEqualTo: pathBox.trailingAnchor, constant: -14),
            
            resetBtn.trailingAnchor.constraint(equalTo: pathBox.trailingAnchor, constant: -14),
            resetBtn.centerYAnchor.constraint(equalTo: vmsDirField.centerYAnchor),
            
            revealBtn.trailingAnchor.constraint(equalTo: resetBtn.leadingAnchor, constant: -8),
            revealBtn.centerYAnchor.constraint(equalTo: resetBtn.centerYAnchor),
            
            chooseBtn.trailingAnchor.constraint(equalTo: revealBtn.leadingAnchor, constant: -8),
            chooseBtn.centerYAnchor.constraint(equalTo: revealBtn.centerYAnchor),
            
            vmsDirField.topAnchor.constraint(equalTo: pathLabel.bottomAnchor, constant: 8),
            vmsDirField.leadingAnchor.constraint(equalTo: pathBox.leadingAnchor, constant: 14),
            vmsDirField.trailingAnchor.constraint(equalTo: chooseBtn.leadingAnchor, constant: -10),
            vmsDirField.heightAnchor.constraint(equalToConstant: 24),
            vmsDirField.bottomAnchor.constraint(equalTo: pathBox.bottomAnchor, constant: -14),
            
            // Stats Box
            statsBox.topAnchor.constraint(equalTo: pathBox.bottomAnchor, constant: 14),
            statsBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            statsBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            statsBox.bottomAnchor.constraint(equalTo: st3Bottom, constant: 12),
            
            // Footer
            footer.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            footer.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            footer.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            footer.heightAnchor.constraint(equalToConstant: 48)
        ])
        
        return view
    }
    
    // MARK: - Images Tab View Builder
    
    private func buildImagesTabView() -> NSView {
        let view = NSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        
        let header = NSTextField(labelWithString: "Operating System Images & ISOs")
        header.translatesAutoresizingMaskIntoConstraints = false
        header.font = NSFont.boldSystemFont(ofSize: 15)
        header.textColor = CubeTokens.primaryText
        view.addSubview(header)
        
        let desc = NSTextField(wrappingLabelWithString: "Installer ISOs and disk templates are cached in ~/Library/Caches/Cube for fast, offline VM creation. Pre-download images or reclaim space.")
        desc.translatesAutoresizingMaskIntoConstraints = false
        desc.font = NSFont.systemFont(ofSize: 12)
        desc.textColor = CubeTokens.secondaryText
        view.addSubview(desc)
        
        // 1. Summary Card
        let summaryBox = createCardBox()
        view.addSubview(summaryBox)
        
        let locLabel = NSTextField(labelWithString: "Cache Location: ~/Library/Caches/Cube")
        locLabel.translatesAutoresizingMaskIntoConstraints = false
        locLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        locLabel.textColor = CubeTokens.secondaryText
        summaryBox.addSubview(locLabel)
        
        imagesTotalUsageLabel = NSTextField(labelWithString: "Calculating cache size...")
        imagesTotalUsageLabel.translatesAutoresizingMaskIntoConstraints = false
        imagesTotalUsageLabel.font = NSFont.systemFont(ofSize: 13, weight: .semibold)
        imagesTotalUsageLabel.textColor = CubeTokens.primaryText
        summaryBox.addSubview(imagesTotalUsageLabel)
        
        let purgeBtn = NSButton(title: "Clear Cache", target: self, action: #selector(clearAllCacheClicked))
        purgeBtn.translatesAutoresizingMaskIntoConstraints = false
        purgeBtn.bezelStyle = .rounded
        purgeBtn.controlSize = .small
        purgeBtn.font = NSFont.systemFont(ofSize: 11)
        summaryBox.addSubview(purgeBtn)
        
        let importBtn = NSButton(title: "Import ISO...", target: self, action: #selector(importLocalISOClicked))
        importBtn.translatesAutoresizingMaskIntoConstraints = false
        importBtn.bezelStyle = .rounded
        importBtn.controlSize = .small
        importBtn.font = NSFont.systemFont(ofSize: 11)
        summaryBox.addSubview(importBtn)
        
        let openFolderBtn = NSButton(title: "Open Folder", target: self, action: #selector(revealCacheFolderClicked))
        openFolderBtn.translatesAutoresizingMaskIntoConstraints = false
        openFolderBtn.bezelStyle = .rounded
        openFolderBtn.controlSize = .small
        openFolderBtn.font = NSFont.systemFont(ofSize: 11)
        summaryBox.addSubview(openFolderBtn)
        
        // 2. Catalog & Cached Images Card
        let catalogBox = createCardBox()
        view.addSubview(catalogBox)
        
        let catalogTitle = NSTextField(labelWithString: "AVAILABLE & CACHED IMAGES")
        catalogTitle.translatesAutoresizingMaskIntoConstraints = false
        catalogTitle.font = CubeTokens.sectionHeaderFont
        catalogTitle.textColor = CubeTokens.secondaryText
        catalogBox.addSubview(catalogTitle)
        
        imagesScrollView = NSScrollView()
        imagesScrollView.translatesAutoresizingMaskIntoConstraints = false
        imagesScrollView.hasVerticalScroller = true
        imagesScrollView.autohidesScrollers = true
        imagesScrollView.borderType = .noBorder
        imagesScrollView.drawsBackground = false
        
        let clipView = FlippedClipView()
        clipView.drawsBackground = false
        imagesScrollView.contentView = clipView
        
        imagesListContainer = NSStackView()
        imagesListContainer.translatesAutoresizingMaskIntoConstraints = false
        imagesListContainer.orientation = .vertical
        imagesListContainer.alignment = .leading
        imagesListContainer.spacing = 0
        imagesScrollView.documentView = imagesListContainer
        catalogBox.addSubview(imagesScrollView)
        
        // 3. Footer
        let footer = buildTabFooter(
            note: "Downloaded ISOs are reused automatically by New Virtual Machine wizards.",
            cancelTitle: "Open Folder",
            cancelAction: #selector(revealCacheFolderClicked),
            applyTitle: "Refresh",
            applyAction: #selector(refreshImagesClicked)
        )
        view.addSubview(footer)
        
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: view.topAnchor, constant: 26),
            header.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            header.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            desc.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 6),
            desc.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            desc.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            // Summary Card
            summaryBox.topAnchor.constraint(equalTo: desc.bottomAnchor, constant: 14),
            summaryBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            summaryBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            locLabel.topAnchor.constraint(equalTo: summaryBox.topAnchor, constant: 12),
            locLabel.leadingAnchor.constraint(equalTo: summaryBox.leadingAnchor, constant: 14),
            
            imagesTotalUsageLabel.topAnchor.constraint(equalTo: locLabel.bottomAnchor, constant: 6),
            imagesTotalUsageLabel.leadingAnchor.constraint(equalTo: summaryBox.leadingAnchor, constant: 14),
            imagesTotalUsageLabel.bottomAnchor.constraint(equalTo: summaryBox.bottomAnchor, constant: -14),
            
            purgeBtn.trailingAnchor.constraint(equalTo: summaryBox.trailingAnchor, constant: -14),
            purgeBtn.centerYAnchor.constraint(equalTo: summaryBox.centerYAnchor),
            
            importBtn.trailingAnchor.constraint(equalTo: purgeBtn.leadingAnchor, constant: -8),
            importBtn.centerYAnchor.constraint(equalTo: summaryBox.centerYAnchor),
            
            openFolderBtn.trailingAnchor.constraint(equalTo: importBtn.leadingAnchor, constant: -8),
            openFolderBtn.centerYAnchor.constraint(equalTo: summaryBox.centerYAnchor),
            
            // Catalog Card
            catalogBox.topAnchor.constraint(equalTo: summaryBox.bottomAnchor, constant: 14),
            catalogBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            catalogBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            catalogBox.bottomAnchor.constraint(equalTo: footer.topAnchor, constant: -14),
            
            catalogTitle.topAnchor.constraint(equalTo: catalogBox.topAnchor, constant: 10),
            catalogTitle.leadingAnchor.constraint(equalTo: catalogBox.leadingAnchor, constant: 14),
            catalogTitle.trailingAnchor.constraint(lessThanOrEqualTo: catalogBox.trailingAnchor, constant: -14),
            
            imagesScrollView.topAnchor.constraint(equalTo: catalogTitle.bottomAnchor, constant: 8),
            imagesScrollView.leadingAnchor.constraint(equalTo: catalogBox.leadingAnchor, constant: 1),
            imagesScrollView.trailingAnchor.constraint(equalTo: catalogBox.trailingAnchor, constant: -1),
            imagesScrollView.bottomAnchor.constraint(equalTo: catalogBox.bottomAnchor, constant: -1),
            
            // List Container inside Scroll View
            imagesListContainer.topAnchor.constraint(equalTo: imagesScrollView.contentView.topAnchor),
            imagesListContainer.leadingAnchor.constraint(equalTo: imagesScrollView.contentView.leadingAnchor),
            imagesListContainer.trailingAnchor.constraint(equalTo: imagesScrollView.contentView.trailingAnchor),
            
            // Footer
            footer.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            footer.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            footer.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            footer.heightAnchor.constraint(equalToConstant: 48)
        ])
        
        return view
    }
    
    private func buildSSHKeysTabView() -> NSView {
        let view = NSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        
        let header = NSTextField(labelWithString: "SSH Key Management")
        header.translatesAutoresizingMaskIntoConstraints = false
        header.font = NSFont.boldSystemFont(ofSize: 15)
        header.textColor = CubeTokens.primaryText
        view.addSubview(header)
        
        let desc = NSTextField(wrappingLabelWithString: "Cube automatically detects your public SSH keys to provision unattended root logins. You can preview, copy, or choose a custom key.")
        desc.translatesAutoresizingMaskIntoConstraints = false
        desc.font = NSFont.systemFont(ofSize: 12)
        desc.textColor = CubeTokens.secondaryText
        view.addSubview(desc)
        
        // Detected Key Card
        let keyBox = createCardBox()
        view.addSubview(keyBox)
        
        sshKeyStatusDot = NSTextField(labelWithString: "●")
        sshKeyStatusDot.translatesAutoresizingMaskIntoConstraints = false
        sshKeyStatusDot.font = NSFont.boldSystemFont(ofSize: 12)
        keyBox.addSubview(sshKeyStatusDot)
        
        sshKeyPathLabel = NSTextField(labelWithString: "~/.ssh/id_ed25519.pub")
        sshKeyPathLabel.translatesAutoresizingMaskIntoConstraints = false
        sshKeyPathLabel.font = NSFont.systemFont(ofSize: 12, weight: .semibold)
        sshKeyPathLabel.textColor = CubeTokens.primaryText
        keyBox.addSubview(sshKeyPathLabel)
        
        sshKeyTypeLabel = NSTextField(labelWithString: "Type: ED25519 (256-bit elliptic curve)")
        sshKeyTypeLabel.translatesAutoresizingMaskIntoConstraints = false
        sshKeyTypeLabel.font = NSFont.systemFont(ofSize: 11)
        sshKeyTypeLabel.textColor = CubeTokens.secondaryText
        keyBox.addSubview(sshKeyTypeLabel)
        
        // Monospace key preview box
        sshKeyPreviewField = NSTextField(wrappingLabelWithString: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5...")
        sshKeyPreviewField.translatesAutoresizingMaskIntoConstraints = false
        sshKeyPreviewField.font = NSFont.monospacedSystemFont(ofSize: 10, weight: .regular)
        sshKeyPreviewField.textColor = CubeTokens.primaryText
        sshKeyPreviewField.isSelectable = true
        keyBox.addSubview(sshKeyPreviewField)
        
        // Buttons
        let copyKeyBtn = NSButton(title: "Copy Public Key", target: self, action: #selector(copySSHKeyClicked(_:)))
        copyKeyBtn.translatesAutoresizingMaskIntoConstraints = false
        copyKeyBtn.bezelStyle = .rounded
        copyKeyBtn.image = NSImage(systemSymbolName: "doc.on.doc", accessibilityDescription: nil)
        copyKeyBtn.imagePosition = .imageLeading
        copyKeyBtn.imageHugsTitle = true
        copyKeyBtn.font = NSFont.systemFont(ofSize: 11)
        keyBox.addSubview(copyKeyBtn)
        
        let revealKeyBtn = NSButton(title: "Reveal in Finder", target: self, action: #selector(revealSSHKeyClicked))
        revealKeyBtn.translatesAutoresizingMaskIntoConstraints = false
        revealKeyBtn.bezelStyle = .rounded
        revealKeyBtn.font = NSFont.systemFont(ofSize: 11)
        keyBox.addSubview(revealKeyBtn)
        
        let chooseKeyBtn = NSButton(title: "Choose Other...", target: self, action: #selector(chooseSSHKeyClicked))
        chooseKeyBtn.translatesAutoresizingMaskIntoConstraints = false
        chooseKeyBtn.bezelStyle = .rounded
        chooseKeyBtn.font = NSFont.systemFont(ofSize: 11)
        keyBox.addSubview(chooseKeyBtn)
        
        let generateKeyBtn = NSButton(title: "Generate Key", target: self, action: #selector(generateSSHKeyClicked))
        generateKeyBtn.translatesAutoresizingMaskIntoConstraints = false
        generateKeyBtn.bezelStyle = .rounded
        generateKeyBtn.font = NSFont.systemFont(ofSize: 11)
        keyBox.addSubview(generateKeyBtn)
        
        // Security note
        let noteBox = createCardBox()
        view.addSubview(noteBox)
        
        let noteIcon = NSImageView()
        noteIcon.translatesAutoresizingMaskIntoConstraints = false
        noteIcon.imageScaling = .scaleProportionallyDown
        let heroConfig = NSImage.SymbolConfiguration(pointSize: 14, weight: .semibold)
        if let lockImg = NSImage(systemSymbolName: "lock.shield", accessibilityDescription: nil)?.withSymbolConfiguration(heroConfig) {
            noteIcon.image = lockImg
            noteIcon.contentTintColor = CubeTokens.accent
        }
        noteBox.addSubview(noteIcon)
        
        let noteText = NSTextField(wrappingLabelWithString: "Security Assurance: Private keys are never read, stored, or transmitted by Cube. Only the public key (.pub) is placed inside /root/.ssh/authorized_keys on the virtual machine for port-forwarded SSH login.")
        noteText.translatesAutoresizingMaskIntoConstraints = false
        noteText.font = NSFont.systemFont(ofSize: 11)
        noteText.textColor = CubeTokens.secondaryText
        noteBox.addSubview(noteText)
        
        // Bottom information bar
        let footer = buildInfoFooter(note: "Public key is automatically injected into new VMs during unattended install.")
        view.addSubview(footer)
        
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: view.topAnchor, constant: 26),
            header.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            header.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            desc.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 6),
            desc.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            desc.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            // Key Box
            keyBox.topAnchor.constraint(equalTo: desc.bottomAnchor, constant: 14),
            keyBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            keyBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            sshKeyStatusDot.topAnchor.constraint(equalTo: keyBox.topAnchor, constant: 12),
            sshKeyStatusDot.leadingAnchor.constraint(equalTo: keyBox.leadingAnchor, constant: 14),
            sshKeyStatusDot.widthAnchor.constraint(equalToConstant: 14),
            
            sshKeyPathLabel.centerYAnchor.constraint(equalTo: sshKeyStatusDot.centerYAnchor),
            sshKeyPathLabel.leadingAnchor.constraint(equalTo: sshKeyStatusDot.trailingAnchor, constant: 4),
            sshKeyPathLabel.trailingAnchor.constraint(lessThanOrEqualTo: keyBox.trailingAnchor, constant: -14),
            
            sshKeyTypeLabel.topAnchor.constraint(equalTo: sshKeyPathLabel.bottomAnchor, constant: 4),
            sshKeyTypeLabel.leadingAnchor.constraint(equalTo: sshKeyPathLabel.leadingAnchor),
            sshKeyTypeLabel.trailingAnchor.constraint(lessThanOrEqualTo: keyBox.trailingAnchor, constant: -14),
            
            sshKeyPreviewField.topAnchor.constraint(equalTo: sshKeyTypeLabel.bottomAnchor, constant: 8),
            sshKeyPreviewField.leadingAnchor.constraint(equalTo: keyBox.leadingAnchor, constant: 14),
            sshKeyPreviewField.trailingAnchor.constraint(equalTo: keyBox.trailingAnchor, constant: -14),
            
            copyKeyBtn.topAnchor.constraint(equalTo: sshKeyPreviewField.bottomAnchor, constant: 12),
            copyKeyBtn.leadingAnchor.constraint(equalTo: keyBox.leadingAnchor, constant: 14),
            copyKeyBtn.bottomAnchor.constraint(equalTo: keyBox.bottomAnchor, constant: -14),
            
            revealKeyBtn.centerYAnchor.constraint(equalTo: copyKeyBtn.centerYAnchor),
            revealKeyBtn.leadingAnchor.constraint(equalTo: copyKeyBtn.trailingAnchor, constant: 8),
            
            chooseKeyBtn.centerYAnchor.constraint(equalTo: copyKeyBtn.centerYAnchor),
            chooseKeyBtn.leadingAnchor.constraint(equalTo: revealKeyBtn.trailingAnchor, constant: 8),
            
            generateKeyBtn.centerYAnchor.constraint(equalTo: copyKeyBtn.centerYAnchor),
            generateKeyBtn.leadingAnchor.constraint(equalTo: chooseKeyBtn.trailingAnchor, constant: 8),
            generateKeyBtn.trailingAnchor.constraint(lessThanOrEqualTo: keyBox.trailingAnchor, constant: -14),
            
            // Note Box - top aligned icon
            noteBox.topAnchor.constraint(equalTo: keyBox.bottomAnchor, constant: 14),
            noteBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            noteBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            noteIcon.topAnchor.constraint(equalTo: noteBox.topAnchor, constant: 12),
            noteIcon.leadingAnchor.constraint(equalTo: noteBox.leadingAnchor, constant: 14),
            noteIcon.widthAnchor.constraint(equalToConstant: 16),
            noteIcon.heightAnchor.constraint(equalToConstant: 16),
            
            noteText.topAnchor.constraint(equalTo: noteBox.topAnchor, constant: 10),
            noteText.leadingAnchor.constraint(equalTo: noteIcon.trailingAnchor, constant: 8),
            noteText.trailingAnchor.constraint(equalTo: noteBox.trailingAnchor, constant: -14),
            noteText.bottomAnchor.constraint(equalTo: noteBox.bottomAnchor, constant: -10),
            
            // Footer
            footer.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            footer.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            footer.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            footer.heightAnchor.constraint(equalToConstant: 48)
        ])
        
        return view
    }
    
    private func buildAboutTabView() -> NSView {
        let view = NSView()
        view.translatesAutoresizingMaskIntoConstraints = false
        
        let iconView = NSImageView()
        iconView.translatesAutoresizingMaskIntoConstraints = false
        if let appIcon = NSApp.applicationIconImage {
            iconView.image = appIcon
        } else {
            let heroConfig = NSImage.SymbolConfiguration(pointSize: 36, weight: .regular)
            if let fallback = NSImage(systemSymbolName: "cube.fill", accessibilityDescription: nil)?.withSymbolConfiguration(heroConfig) {
                iconView.image = fallback
                iconView.contentTintColor = CubeTokens.accent
            }
        }
        view.addSubview(iconView)
        
        let appTitle = NSTextField(labelWithString: "Cube")
        appTitle.translatesAutoresizingMaskIntoConstraints = false
        appTitle.font = CubeTokens.heroTitleFont
        appTitle.textColor = CubeTokens.primaryText
        view.addSubview(appTitle)
        
        let versionLabel = NSTextField(labelWithString: "Version \(AppVersion.displayString)")
        versionLabel.translatesAutoresizingMaskIntoConstraints = false
        versionLabel.font = NSFont.systemFont(ofSize: 12)
        versionLabel.textColor = CubeTokens.secondaryText
        view.addSubview(versionLabel)
        
        // Platform Diagnostics Card
        let statusBox = createCardBox()
        view.addSubview(statusBox)
        
        let (_, st1) = addInfoRow(label: "Virtualization Engine:", value: "Apple Silicon Hypervisor.framework (ARM64)", in: statusBox, topAnchor: statusBox.topAnchor, topOffset: 12)
        let netEntitled = VMManager.shared.hasNetworkingEntitlement() ? "Granted (com.apple.vm.networking)" : "Local Proxy Fallback (vmnet-proxy)"
        let (_, st2) = addInfoRow(label: "Network Entitlement:", value: netEntitled, in: statusBox, topAnchor: st1, topOffset: 8)
        let (_, st3) = addInfoRow(label: "CLI Utilities:", value: "cubectl (manager), cube (vmm)", in: statusBox, topAnchor: st2, topOffset: 8)
        let (_, st4) = addInfoRow(label: "Target Guest OS:", value: "FreeBSD ARM64 (RELEASE / CURRENT)", in: statusBox, topAnchor: st3, topOffset: 8)
        
        let creditsText = NSTextField(wrappingLabelWithString: "Cube is a lightweight native macOS virtual machine manager built specifically for ARM64 guest systems, powered by Hypervisor.framework, virtio, and UEFI.")
        creditsText.translatesAutoresizingMaskIntoConstraints = false
        creditsText.font = NSFont.systemFont(ofSize: 11)
        creditsText.textColor = CubeTokens.secondaryText
        view.addSubview(creditsText)
        
        // Bottom footer
        let footer = buildInfoFooter(note: "Cube Virtual Machine Manager — \(AppVersion.displayString)")
        view.addSubview(footer)
        
        NSLayoutConstraint.activate([
            iconView.topAnchor.constraint(equalTo: view.topAnchor, constant: 26),
            iconView.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            iconView.widthAnchor.constraint(equalToConstant: 48),
            iconView.heightAnchor.constraint(equalToConstant: 48),
            
            appTitle.topAnchor.constraint(equalTo: view.topAnchor, constant: 24),
            appTitle.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 14),
            appTitle.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            versionLabel.topAnchor.constraint(equalTo: appTitle.bottomAnchor, constant: 2),
            versionLabel.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 14),
            versionLabel.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -24),
            
            // Status Card
            statusBox.topAnchor.constraint(equalTo: iconView.bottomAnchor, constant: 18),
            statusBox.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            statusBox.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            statusBox.bottomAnchor.constraint(equalTo: st4, constant: 12),
            
            creditsText.topAnchor.constraint(equalTo: statusBox.bottomAnchor, constant: 16),
            creditsText.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 24),
            creditsText.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -24),
            
            // Footer
            footer.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            footer.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            footer.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            footer.heightAnchor.constraint(equalToConstant: 48)
        ])
        
        return view
    }
    
    // MARK: - Footer Builder Helper
    
    private func buildTabFooter(
        note: String?,
        cancelTitle: String = "Cancel",
        cancelAction: Selector?,
        applyTitle: String,
        applyAction: Selector
    ) -> NSView {
        let footer = NSView()
        footer.translatesAutoresizingMaskIntoConstraints = false
        
        let sep = NSBox()
        sep.translatesAutoresizingMaskIntoConstraints = false
        sep.boxType = .custom
        sep.borderWidth = 0
        sep.fillColor = CubeTokens.separatorColor
        footer.addSubview(sep)
        
        let applyBtn = NSButton(title: applyTitle, target: self, action: applyAction)
        applyBtn.translatesAutoresizingMaskIntoConstraints = false
        applyBtn.bezelStyle = .rounded
        applyBtn.keyEquivalent = "\r"
        footer.addSubview(applyBtn)
        
        var trailingForNote = applyBtn.leadingAnchor
        var cancelBtn: NSButton? = nil
        if let cancelAction = cancelAction {
            let btn = NSButton(title: cancelTitle, target: self, action: cancelAction)
            btn.translatesAutoresizingMaskIntoConstraints = false
            btn.bezelStyle = .rounded
            footer.addSubview(btn)
            cancelBtn = btn
            trailingForNote = btn.leadingAnchor
        }
        
        var noteLabel: NSTextField? = nil
        if let note = note {
            let lbl = NSTextField(labelWithString: note)
            lbl.translatesAutoresizingMaskIntoConstraints = false
            lbl.font = NSFont.systemFont(ofSize: 11)
            lbl.textColor = CubeTokens.secondaryText
            lbl.lineBreakMode = .byTruncatingTail
            footer.addSubview(lbl)
            noteLabel = lbl
        }
        
        var constraints: [NSLayoutConstraint] = [
            sep.topAnchor.constraint(equalTo: footer.topAnchor),
            sep.leadingAnchor.constraint(equalTo: footer.leadingAnchor),
            sep.trailingAnchor.constraint(equalTo: footer.trailingAnchor),
            sep.heightAnchor.constraint(equalToConstant: 1),
            
            applyBtn.trailingAnchor.constraint(equalTo: footer.trailingAnchor, constant: -24),
            applyBtn.centerYAnchor.constraint(equalTo: footer.centerYAnchor),
            applyBtn.heightAnchor.constraint(equalToConstant: 30)
        ]
        
        if let cancelBtn = cancelBtn {
            constraints.append(contentsOf: [
                cancelBtn.trailingAnchor.constraint(equalTo: applyBtn.leadingAnchor, constant: -10),
                cancelBtn.centerYAnchor.constraint(equalTo: footer.centerYAnchor),
                cancelBtn.heightAnchor.constraint(equalToConstant: 30)
            ])
        }
        
        if let noteLabel = noteLabel {
            constraints.append(contentsOf: [
                noteLabel.leadingAnchor.constraint(equalTo: footer.leadingAnchor, constant: 24),
                noteLabel.centerYAnchor.constraint(equalTo: footer.centerYAnchor),
                noteLabel.trailingAnchor.constraint(lessThanOrEqualTo: trailingForNote, constant: -12)
            ])
        }
        
        NSLayoutConstraint.activate(constraints)
        return footer
    }
    
    private func buildInfoFooter(note: String) -> NSView {
        let footer = NSView()
        footer.translatesAutoresizingMaskIntoConstraints = false
        
        let sep = NSBox()
        sep.translatesAutoresizingMaskIntoConstraints = false
        sep.boxType = .custom
        sep.borderWidth = 0
        sep.fillColor = CubeTokens.separatorColor
        footer.addSubview(sep)
        
        let noteLabel = NSTextField(labelWithString: note)
        noteLabel.translatesAutoresizingMaskIntoConstraints = false
        noteLabel.font = NSFont.systemFont(ofSize: 11)
        noteLabel.textColor = CubeTokens.secondaryText
        noteLabel.lineBreakMode = .byTruncatingTail
        footer.addSubview(noteLabel)
        
        NSLayoutConstraint.activate([
            sep.topAnchor.constraint(equalTo: footer.topAnchor),
            sep.leadingAnchor.constraint(equalTo: footer.leadingAnchor),
            sep.trailingAnchor.constraint(equalTo: footer.trailingAnchor),
            sep.heightAnchor.constraint(equalToConstant: 1),
            
            noteLabel.leadingAnchor.constraint(equalTo: footer.leadingAnchor, constant: 24),
            noteLabel.centerYAnchor.constraint(equalTo: footer.centerYAnchor),
            noteLabel.trailingAnchor.constraint(lessThanOrEqualTo: footer.trailingAnchor, constant: -24)
        ])
        
        return footer
    }
    
    // MARK: - Sidebar Selection & Tab Switching
    
    private func switchTab(id: String) {
        currentTabId = id
        for subview in contentContainerView.subviews {
            subview.removeFromSuperview()
        }
        if let tabView = tabViews[id] {
            tabView.translatesAutoresizingMaskIntoConstraints = false
            contentContainerView.addSubview(tabView)
            NSLayoutConstraint.activate([
                tabView.topAnchor.constraint(equalTo: contentContainerView.topAnchor),
                tabView.bottomAnchor.constraint(equalTo: contentContainerView.bottomAnchor),
                tabView.leadingAnchor.constraint(equalTo: contentContainerView.leadingAnchor),
                tabView.trailingAnchor.constraint(equalTo: contentContainerView.trailingAnchor)
            ])
            if CubeTokens.reduceMotion {
                tabView.alphaValue = 1.0
            } else {
                tabView.alphaValue = 0.0
                NSAnimationContext.runAnimationGroup { ctx in
                    ctx.duration = 0.12
                    tabView.animator().alphaValue = 1.0
                }
            }
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
            let v = NSView()
            v.identifier = cellId
            
            let iconView = NSImageView()
            iconView.translatesAutoresizingMaskIntoConstraints = false
            iconView.tag = 201
            v.addSubview(iconView)
            
            let label = NSTextField(labelWithString: "")
            label.translatesAutoresizingMaskIntoConstraints = false
            label.tag = 202
            label.font = NSFont.systemFont(ofSize: 13, weight: .medium)
            label.textColor = CubeTokens.primaryText
            v.addSubview(label)
            
            NSLayoutConstraint.activate([
                iconView.leadingAnchor.constraint(equalTo: v.leadingAnchor, constant: 12),
                iconView.centerYAnchor.constraint(equalTo: v.centerYAnchor),
                iconView.widthAnchor.constraint(equalToConstant: 18),
                iconView.heightAnchor.constraint(equalToConstant: 18),
                
                label.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 8),
                label.centerYAnchor.constraint(equalTo: v.centerYAnchor),
                label.trailingAnchor.constraint(equalTo: v.trailingAnchor, constant: -8)
            ])
            
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
                case "images": iconView.contentTintColor = .systemIndigo
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
    
    private func addDiagRow(
        label: String,
        value: String,
        in card: CardView,
        topAnchor: NSLayoutYAxisAnchor,
        topOffset: CGFloat = 10,
        copyAction: Selector?
    ) -> (NSTextField, NSLayoutYAxisAnchor) {
        let lbl = NSTextField(labelWithString: label)
        lbl.translatesAutoresizingMaskIntoConstraints = false
        lbl.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        lbl.textColor = CubeTokens.secondaryText
        card.addSubview(lbl)
        
        let val = NSTextField(labelWithString: value)
        val.translatesAutoresizingMaskIntoConstraints = false
        val.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        val.textColor = CubeTokens.primaryText
        val.isSelectable = true
        card.addSubview(val)
        
        var trailingTarget = card.trailingAnchor
        var trailingOffset: CGFloat = -14
        
        if let action = copyAction {
            let copyBtn = NSButton(title: "Copy", target: self, action: action)
            copyBtn.translatesAutoresizingMaskIntoConstraints = false
            copyBtn.bezelStyle = .rounded
            copyBtn.controlSize = .small
            copyBtn.image = NSImage(systemSymbolName: "doc.on.doc", accessibilityDescription: nil)
            copyBtn.imagePosition = .imageLeading
            copyBtn.imageHugsTitle = true
            copyBtn.font = NSFont.systemFont(ofSize: 10)
            card.addSubview(copyBtn)
            
            NSLayoutConstraint.activate([
                copyBtn.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
                copyBtn.centerYAnchor.constraint(equalTo: lbl.centerYAnchor),
                copyBtn.widthAnchor.constraint(equalToConstant: 70),
                copyBtn.heightAnchor.constraint(equalToConstant: 21)
            ])
            trailingTarget = copyBtn.leadingAnchor
            trailingOffset = -8
        }
        
        NSLayoutConstraint.activate([
            lbl.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            lbl.topAnchor.constraint(equalTo: topAnchor, constant: topOffset),
            lbl.widthAnchor.constraint(equalToConstant: 120),
            
            val.leadingAnchor.constraint(equalTo: lbl.trailingAnchor, constant: 8),
            val.centerYAnchor.constraint(equalTo: lbl.centerYAnchor),
            val.trailingAnchor.constraint(lessThanOrEqualTo: trailingTarget, constant: trailingOffset)
        ])
        
        return (val, lbl.bottomAnchor)
    }
    
    private func addInfoRow(
        label: String,
        value: String,
        in card: CardView,
        topAnchor: NSLayoutYAxisAnchor,
        topOffset: CGFloat = 10
    ) -> (NSTextField, NSLayoutYAxisAnchor) {
        let lbl = NSTextField(labelWithString: label)
        lbl.translatesAutoresizingMaskIntoConstraints = false
        lbl.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        lbl.textColor = CubeTokens.secondaryText
        card.addSubview(lbl)
        
        let val = NSTextField(labelWithString: value)
        val.translatesAutoresizingMaskIntoConstraints = false
        val.font = NSFont.systemFont(ofSize: 11, weight: .regular)
        val.textColor = CubeTokens.primaryText
        val.isSelectable = true
        card.addSubview(val)
        
        NSLayoutConstraint.activate([
            lbl.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            lbl.topAnchor.constraint(equalTo: topAnchor, constant: topOffset),
            lbl.widthAnchor.constraint(equalToConstant: 175),
            
            val.leadingAnchor.constraint(equalTo: lbl.trailingAnchor, constant: 6),
            val.centerYAnchor.constraint(equalTo: lbl.centerYAnchor),
            val.trailingAnchor.constraint(lessThanOrEqualTo: card.trailingAnchor, constant: -14)
        ])
        
        return (val, lbl.bottomAnchor)
    }
    
    private func addShortcutRow(
        action: String,
        shortcut: String,
        in card: CardView,
        topAnchor: NSLayoutYAxisAnchor,
        topOffset: CGFloat = 10
    ) -> (NSTextField, NSLayoutYAxisAnchor) {
        let lbl = NSTextField(labelWithString: action)
        lbl.translatesAutoresizingMaskIntoConstraints = false
        lbl.font = NSFont.systemFont(ofSize: 12, weight: .medium)
        lbl.textColor = CubeTokens.primaryText
        card.addSubview(lbl)
        
        let val = NSTextField(labelWithString: shortcut)
        val.translatesAutoresizingMaskIntoConstraints = false
        val.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .semibold)
        val.textColor = CubeTokens.secondaryText
        val.alignment = .right
        card.addSubview(val)
        
        NSLayoutConstraint.activate([
            lbl.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            lbl.topAnchor.constraint(equalTo: topAnchor, constant: topOffset),
            lbl.widthAnchor.constraint(equalToConstant: 220),
            
            val.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            val.centerYAnchor.constraint(equalTo: lbl.centerYAnchor),
            val.leadingAnchor.constraint(greaterThanOrEqualTo: lbl.trailingAnchor, constant: 10)
        ])
        
        return (val, lbl.bottomAnchor)
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
        loadImagesState()
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
    
    @objc private func copyIPv4Clicked(_ sender: Any?) {
        guard !lastIPv4.isEmpty else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(lastIPv4, forType: .string)
        if let btn = sender as? NSButton {
            CubeTokens.animateCopyFeedback(button: btn)
        }
    }
    
    @objc private func copyIPv6Clicked(_ sender: Any?) {
        guard !lastIPv6.isEmpty else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(lastIPv6, forType: .string)
        if let btn = sender as? NSButton {
            CubeTokens.animateCopyFeedback(button: btn)
        }
    }
    
    @objc private func copySocketClicked(_ sender: Any?) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(ServiceManager.shared.defaultSocketPath, forType: .string)
        if let btn = sender as? NSButton {
            CubeTokens.animateCopyFeedback(button: btn)
        }
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
            CubeTokens.animateCopyFeedback(
                button: btn,
                temporaryTitle: "Copied!",
                temporaryIcon: "checkmark",
                originalTitle: "Copy Public Key",
                originalIcon: "doc.on.doc",
                duration: 1.2
            )
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
    
    // MARK: - Images Tab Management
    
    private func loadImagesState() {
        refreshImagesUI()
    }
    
    private func refreshImagesUI() {
        guard imagesTotalUsageLabel != nil, imagesListContainer != nil, imagesScrollView != nil else { return }
        
        let items = ImageCacheManager.shared.getAllImageItems()
        let totalBytes = ImageCacheManager.shared.getTotalCacheBytes()
        let formattedTotal = ImageCacheManager.formatBytes(totalBytes)
        let count = items.filter { $0.isCached }.count
        imagesTotalUsageLabel.stringValue = "\(formattedTotal) used (\(count) cached file\(count == 1 ? "" : "s"))"
        
        for sv in imagesListContainer.arrangedSubviews {
            imagesListContainer.removeArrangedSubview(sv)
            sv.removeFromSuperview()
        }
        
        for (idx, item) in items.enumerated() {
            let row = buildImageRowView(item: item, isLast: idx == items.count - 1)
            imagesListContainer.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: imagesListContainer.widthAnchor).isActive = true
        }
    }
    
    private func buildImageRowView(item: OSImageItem, isLast: Bool) -> NSView {
        let row = NSView()
        row.translatesAutoresizingMaskIntoConstraints = false
        row.heightAnchor.constraint(equalToConstant: 58).isActive = true
        
        // Icon
        let iconView = NSImageView()
        iconView.translatesAutoresizingMaskIntoConstraints = false
        let cfg = NSImage.SymbolConfiguration(pointSize: 20, weight: .regular)
        if let img = NSImage(systemSymbolName: "opticaldisc", accessibilityDescription: nil)?.withSymbolConfiguration(cfg) {
            iconView.image = img
            iconView.contentTintColor = item.isCached ? .systemGreen : (item.isDownloading ? .systemBlue : .secondaryLabelColor)
        }
        row.addSubview(iconView)
        
        // Title
        let titleLabel = NSTextField(labelWithString: item.title)
        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.font = NSFont.systemFont(ofSize: 13, weight: .semibold)
        titleLabel.textColor = .labelColor
        row.addSubview(titleLabel)
        
        // Subtitle (Filename)
        let subLabel = NSTextField(labelWithString: item.filename)
        subLabel.translatesAutoresizingMaskIntoConstraints = false
        subLabel.font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .regular)
        subLabel.textColor = .secondaryLabelColor
        subLabel.lineBreakMode = .byTruncatingMiddle
        subLabel.toolTip = item.filename
        row.addSubview(subLabel)
        
        var trailingRef = row.trailingAnchor
        var trailingPadding: CGFloat = -12

        if item.isDownloading {
            let cancelBtn = ActionButton(title: "Cancel")
            cancelBtn.bezelStyle = .rounded
            cancelBtn.controlSize = .small
            cancelBtn.font = NSFont.systemFont(ofSize: 11)
            cancelBtn.onClick = { [weak self] _ in
                ImageCacheManager.shared.cancelDownload(presetId: item.id)
                self?.refreshImagesUI()
            }
            row.addSubview(cancelBtn)

            let progressIndicator = NSProgressIndicator()
            progressIndicator.translatesAutoresizingMaskIntoConstraints = false
            progressIndicator.isIndeterminate = false
            progressIndicator.minValue = 0.0
            progressIndicator.maxValue = 1.0
            progressIndicator.doubleValue = item.downloadProgress
            row.addSubview(progressIndicator)
            
            let pctLabel = NSTextField(labelWithString: "\(Int(item.downloadProgress * 100))%")
            pctLabel.translatesAutoresizingMaskIntoConstraints = false
            pctLabel.font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .medium)
            pctLabel.textColor = .systemBlue
            pctLabel.alignment = .right
            row.addSubview(pctLabel)

            NSLayoutConstraint.activate([
                cancelBtn.centerYAnchor.constraint(equalTo: row.centerYAnchor),
                cancelBtn.trailingAnchor.constraint(equalTo: row.trailingAnchor, constant: -12),
                cancelBtn.widthAnchor.constraint(equalToConstant: 66),
                cancelBtn.heightAnchor.constraint(equalToConstant: 24),

                progressIndicator.trailingAnchor.constraint(equalTo: cancelBtn.leadingAnchor, constant: -10),
                progressIndicator.topAnchor.constraint(equalTo: row.topAnchor, constant: 16),
                progressIndicator.widthAnchor.constraint(equalToConstant: 100),
                progressIndicator.heightAnchor.constraint(equalToConstant: 12),

                pctLabel.trailingAnchor.constraint(equalTo: cancelBtn.leadingAnchor, constant: -10),
                pctLabel.topAnchor.constraint(equalTo: progressIndicator.bottomAnchor, constant: 2),
                pctLabel.widthAnchor.constraint(equalToConstant: 100)
            ])
            trailingRef = progressIndicator.leadingAnchor
            trailingPadding = -8
        } else if item.isCached {
            let deleteBtn = ActionButton(title: "Delete")
            deleteBtn.bezelStyle = .rounded
            deleteBtn.controlSize = .small
            deleteBtn.font = NSFont.systemFont(ofSize: 11)
            deleteBtn.onClick = { [weak self] _ in
                let alert = NSAlert()
                alert.messageText = "Delete Cached Image?"
                alert.informativeText = "Are you sure you want to delete \(item.filename) from the cache?"
                alert.alertStyle = .warning
                alert.addButton(withTitle: "Delete")
                alert.addButton(withTitle: "Cancel")
                if alert.runModal() == .alertFirstButtonReturn {
                    _ = ImageCacheManager.shared.deleteImage(filename: item.filename)
                    self?.refreshImagesUI()
                }
            }
            row.addSubview(deleteBtn)

            let revealBtn = ActionButton(title: "Reveal")
            revealBtn.bezelStyle = .rounded
            revealBtn.controlSize = .small
            revealBtn.font = NSFont.systemFont(ofSize: 11)
            revealBtn.onClick = { _ in
                let path = "\(ImageCacheManager.shared.cacheDir)/\(item.filename)"
                if FileManager.default.fileExists(atPath: path) {
                    NSWorkspace.shared.selectFile(path, inFileViewerRootedAtPath: "")
                }
            }
            row.addSubview(revealBtn)

            let badge = NSTextField(labelWithString: "● CACHED")
            badge.translatesAutoresizingMaskIntoConstraints = false
            badge.font = NSFont.systemFont(ofSize: 9.5, weight: .bold)
            badge.textColor = .systemGreen
            badge.alignment = .right
            row.addSubview(badge)

            let sizeLabel = NSTextField(labelWithString: item.sizeDisplay)
            sizeLabel.translatesAutoresizingMaskIntoConstraints = false
            sizeLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
            sizeLabel.textColor = .labelColor
            sizeLabel.alignment = .right
            row.addSubview(sizeLabel)

            NSLayoutConstraint.activate([
                deleteBtn.centerYAnchor.constraint(equalTo: row.centerYAnchor),
                deleteBtn.trailingAnchor.constraint(equalTo: row.trailingAnchor, constant: -12),
                deleteBtn.widthAnchor.constraint(equalToConstant: 62),
                deleteBtn.heightAnchor.constraint(equalToConstant: 24),

                revealBtn.centerYAnchor.constraint(equalTo: row.centerYAnchor),
                revealBtn.trailingAnchor.constraint(equalTo: deleteBtn.leadingAnchor, constant: -6),
                revealBtn.widthAnchor.constraint(equalToConstant: 60),
                revealBtn.heightAnchor.constraint(equalToConstant: 24),

                badge.trailingAnchor.constraint(equalTo: revealBtn.leadingAnchor, constant: -10),
                badge.topAnchor.constraint(equalTo: row.topAnchor, constant: 14),
                badge.widthAnchor.constraint(equalToConstant: 80),

                sizeLabel.trailingAnchor.constraint(equalTo: revealBtn.leadingAnchor, constant: -10),
                sizeLabel.topAnchor.constraint(equalTo: badge.bottomAnchor, constant: 2),
                sizeLabel.widthAnchor.constraint(equalToConstant: 80)
            ])
            trailingRef = badge.leadingAnchor
            trailingPadding = -8
        } else {
            let dlBtn = ActionButton(title: "Download")
            dlBtn.bezelStyle = .rounded
            dlBtn.controlSize = .small
            dlBtn.font = NSFont.systemFont(ofSize: 11, weight: .semibold)
            dlBtn.onClick = { [weak self] _ in
                ImageCacheManager.shared.startDownload(presetId: item.id)
                self?.refreshImagesUI()
            }
            row.addSubview(dlBtn)

            let badge = NSTextField(labelWithString: "○ AVAILABLE")
            badge.translatesAutoresizingMaskIntoConstraints = false
            badge.font = NSFont.systemFont(ofSize: 9.5, weight: .bold)
            badge.textColor = .tertiaryLabelColor
            badge.alignment = .right
            row.addSubview(badge)

            let sizeLabel = NSTextField(labelWithString: "~\(item.fileSize == nil ? "1.15 GB" : item.sizeDisplay)")
            sizeLabel.translatesAutoresizingMaskIntoConstraints = false
            sizeLabel.font = NSFont.systemFont(ofSize: 11)
            sizeLabel.textColor = .secondaryLabelColor
            sizeLabel.alignment = .right
            row.addSubview(sizeLabel)

            NSLayoutConstraint.activate([
                dlBtn.centerYAnchor.constraint(equalTo: row.centerYAnchor),
                dlBtn.trailingAnchor.constraint(equalTo: row.trailingAnchor, constant: -12),
                dlBtn.widthAnchor.constraint(equalToConstant: 84),
                dlBtn.heightAnchor.constraint(equalToConstant: 24),

                badge.trailingAnchor.constraint(equalTo: dlBtn.leadingAnchor, constant: -10),
                badge.topAnchor.constraint(equalTo: row.topAnchor, constant: 14),
                badge.widthAnchor.constraint(equalToConstant: 85),

                sizeLabel.trailingAnchor.constraint(equalTo: dlBtn.leadingAnchor, constant: -10),
                sizeLabel.topAnchor.constraint(equalTo: badge.bottomAnchor, constant: 2),
                sizeLabel.widthAnchor.constraint(equalToConstant: 85)
            ])
            trailingRef = badge.leadingAnchor
            trailingPadding = -8
        }

        NSLayoutConstraint.activate([
            iconView.leadingAnchor.constraint(equalTo: row.leadingAnchor, constant: 14),
            iconView.centerYAnchor.constraint(equalTo: row.centerYAnchor),
            iconView.widthAnchor.constraint(equalToConstant: 24),
            iconView.heightAnchor.constraint(equalToConstant: 24),

            titleLabel.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 10),
            titleLabel.topAnchor.constraint(equalTo: row.topAnchor, constant: 10),
            titleLabel.trailingAnchor.constraint(lessThanOrEqualTo: trailingRef, constant: trailingPadding),

            subLabel.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 10),
            subLabel.topAnchor.constraint(equalTo: titleLabel.bottomAnchor, constant: 2),
            subLabel.trailingAnchor.constraint(lessThanOrEqualTo: trailingRef, constant: trailingPadding)
        ])

        if !isLast {
            let sep = NSBox()
            sep.translatesAutoresizingMaskIntoConstraints = false
            sep.boxType = .custom
            sep.borderWidth = 0
            sep.fillColor = CubeTokens.separatorColor
            row.addSubview(sep)

            NSLayoutConstraint.activate([
                sep.leadingAnchor.constraint(equalTo: row.leadingAnchor, constant: 12),
                sep.trailingAnchor.constraint(equalTo: row.trailingAnchor, constant: -12),
                sep.bottomAnchor.constraint(equalTo: row.bottomAnchor),
                sep.heightAnchor.constraint(equalToConstant: 1)
            ])
        }

        return row
    }
    
    @objc private func revealCacheFolderClicked() {
        let path = ImageCacheManager.shared.cacheDir
        ImageCacheManager.shared.ensureCacheDirectoryExists()
        NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: path)
    }

    @objc private func refreshImagesClicked() {
        refreshImagesUI()
    }

    @objc private func importLocalISOClicked() {
        guard let win = self.window else { return }
        let panel = NSOpenPanel()
        panel.title = "Select OS Image or ISO to Import"
        panel.prompt = "Import"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [
            UTType(filenameExtension: "iso") ?? .data,
            UTType(filenameExtension: "img") ?? .data,
            UTType(filenameExtension: "raw") ?? .data,
            UTType(filenameExtension: "qcow2") ?? .data
        ]
        
        panel.beginSheetModal(for: win) { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            do {
                _ = try ImageCacheManager.shared.importLocalISO(from: url)
                self?.refreshImagesUI()
            } catch {
                let alert = NSAlert()
                alert.messageText = "Import Failed"
                alert.informativeText = error.localizedDescription
                alert.alertStyle = .warning
                alert.runModal()
            }
        }
    }

    @objc private func clearAllCacheClicked() {
        let total = ImageCacheManager.shared.getTotalCacheBytes()
        if total == 0 {
            let alert = NSAlert()
            alert.messageText = "Cache is Empty"
            alert.informativeText = "There are currently no cached images in ~/Library/Caches/Cube."
            alert.alertStyle = .informational
            alert.runModal()
            return
        }
        
        let alert = NSAlert()
        alert.messageText = "Clear All Cached Images?"
        alert.informativeText = "This will permanently delete all installer ISOs and free up \(ImageCacheManager.formatBytes(total)) of disk space. You can re-download them at any time."
        alert.alertStyle = .critical
        alert.addButton(withTitle: "Clear Cache")
        alert.addButton(withTitle: "Cancel")
        
        let res = alert.runModal()
        if res == .alertFirstButtonReturn {
            _ = ImageCacheManager.shared.clearAllCache()
            refreshImagesUI()
        }
    }

    @objc private func startImageDownloadClicked(_ sender: NSButton) {
        guard let presetId = sender.identifier?.rawValue else { return }
        ImageCacheManager.shared.startDownload(presetId: presetId)
        refreshImagesUI()
    }

    @objc private func cancelImageDownloadClicked(_ sender: NSButton) {
        guard let presetId = sender.identifier?.rawValue else { return }
        ImageCacheManager.shared.cancelDownload(presetId: presetId)
        refreshImagesUI()
    }

    @objc private func revealImageClicked(_ sender: NSButton) {
        guard let filename = sender.identifier?.rawValue else { return }
        let path = "\(ImageCacheManager.shared.cacheDir)/\(filename)"
        if FileManager.default.fileExists(atPath: path) {
            NSWorkspace.shared.selectFile(path, inFileViewerRootedAtPath: "")
        }
    }

    @objc private func deleteImageClicked(_ sender: NSButton) {
        guard let filename = sender.identifier?.rawValue else { return }
        let path = "\(ImageCacheManager.shared.cacheDir)/\(filename)"
        var fileSizeStr = ""
        if let attr = try? FileManager.default.attributesOfItem(atPath: path),
           let num = attr[.size] as? NSNumber {
            fileSizeStr = " (\(ImageCacheManager.formatBytes(num.int64Value)))"
        }
        
        let alert = NSAlert()
        alert.messageText = "Delete Cached Image?"
        alert.informativeText = "Are you sure you want to delete '\(filename)'\(fileSizeStr)? It can be re-downloaded when needed."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        
        if alert.runModal() == .alertFirstButtonReturn {
            _ = ImageCacheManager.shared.deleteImage(filename: filename)
            refreshImagesUI()
        }
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

    // MARK: - Accessibility Adaptations

    @objc private func accessibilityDisplayOptionsDidChange(_ notification: Notification) {
        updateAccessibilityPreferences()
    }

    private func updateAccessibilityPreferences() {
        let reduceTransparency = CubeTokens.reduceTransparency
        guard let window = self.window else { return }

        window.isOpaque = reduceTransparency
        window.backgroundColor = reduceTransparency ? .windowBackgroundColor : .clear

        rootVisualEffect?.material = reduceTransparency ? .windowBackground : .underWindowBackground
        rootVisualEffect?.blendingMode = reduceTransparency ? .withinWindow : .behindWindow
        rootVisualEffect?.state = reduceTransparency ? .inactive : .active
        rootVisualEffect?.layer?.borderColor = CubeTokens.windowBorder.cgColor

        sidebarContainer?.material = reduceTransparency ? .windowBackground : .sidebar
        sidebarContainer?.blendingMode = reduceTransparency ? .withinWindow : .behindWindow
        sidebarContainer?.state = reduceTransparency ? .inactive : .active

        refreshAllCardBoxes(in: window.contentView)
        window.contentView?.needsDisplay = true
    }

    private func refreshAllCardBoxes(in view: NSView?) {
        guard let view = view else { return }
        if let card = view as? CardView {
            card.updateColors()
            card.needsDisplay = true
        }
        for sub in view.subviews {
            refreshAllCardBoxes(in: sub)
        }
    }
}
