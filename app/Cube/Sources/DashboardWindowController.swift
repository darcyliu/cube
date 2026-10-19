import Foundation
import AppKit

private class DashboardFlippedView: NSView {
    override var isFlipped: Bool { return true }
}

private class VerticallyCenteredTextFieldCell: NSTextFieldCell {
    override func drawingRect(forBounds rect: NSRect) -> NSRect {
        let newRect = super.drawingRect(forBounds: rect)
        let textSize = cellSize(forBounds: rect)
        let deltaY = (rect.height - textSize.height) / 2.0
        if deltaY > 0 {
            return NSRect(x: newRect.origin.x, y: newRect.origin.y + deltaY, width: newRect.width, height: textSize.height)
        }
        return newRect
    }
}

// MARK: - Cube Design System Tokens
private enum CubeTokens {
    // Colors
    static let cardBackground = NSColor.controlBackgroundColor
    static let cardBorder = NSColor.separatorColor
    static let accent = NSColor.systemBlue
    static let runningGreen = NSColor(red: 0.20, green: 0.78, blue: 0.35, alpha: 1.0)
    static let stoppedGray = NSColor.secondaryLabelColor
    static let primaryText = NSColor.labelColor
    static let secondaryText = NSColor.secondaryLabelColor
    static let tertiaryText = NSColor.tertiaryLabelColor
    static let subtleTagBg = NSColor.quaternaryLabelColor.withAlphaComponent(0.08)

    // Typography
    static let heroTitleFont = NSFont.systemFont(ofSize: 21, weight: .bold)
    static let sectionHeaderFont = NSFont.systemFont(ofSize: 11, weight: .semibold)
    static let bodyLabelFont = NSFont.systemFont(ofSize: 11.5, weight: .medium)
    static let bodyValueFont = NSFont.monospacedSystemFont(ofSize: 11.5, weight: .regular)
    static let badgeFont = NSFont.monospacedSystemFont(ofSize: 10, weight: .semibold)
    static let sidebarTitleFont = NSFont.systemFont(ofSize: 13, weight: .semibold)
    static let sidebarSubtitleFont = NSFont.systemFont(ofSize: 10.5, weight: .regular)

    // Layout Metrics & Radii
    static let cardCornerRadius: CGFloat = 10
    static let badgeCornerRadius: CGFloat = 5
    static let cardBorderWidth: CGFloat = 1.0
}

class DashboardWindowController: NSWindowController, NSWindowDelegate, NSTableViewDataSource, NSTableViewDelegate {
    static let shared = DashboardWindowController()

    private var splitView: NSSplitView!
    private var sidebarView: NSView!
    private var detailContainerView: NSView!

    // Sidebar components
    private var tableView: NSTableView!
    private var vmCountLabel: NSTextField!

    // Detail components - Empty State
    private var emptyStateView: NSView!

    // Detail components - Active VM
    private var detailView: DashboardFlippedView!
    private var vmNameLabel: NSTextField!
    private var statusBadge: NSTextField!
    private var startStopButton: NSButton!
    private var sshButton: NSButton!
    private var vncButton: NSButton!
    private var logsButton: NSButton!
    private var deleteButton: NSButton!

    // Hardware Card
    private var osVersionLabel: NSTextField!
    private var archLabel: NSTextField!
    private var cpusLabel: NSTextField!
    private var ramLabel: NSTextField!
    private var bootModeLabel: NSTextField!
    private var diskPathLabel: NSTextField!

    // Network Card
    private var netModeLabel: NSTextField!
    private var ipLabel: NSTextField!
    private var sshCommandLabel: NSTextField!
    private var vncLabel: NSTextField!
    private var sharesListLabel: NSTextField!
    private var copyMountCmdButton: NSButton!
    private var manageSharesButton: NSButton!

    // Empty & Welcome State
    private var emptyTitleLabel: NSTextField!
    private var emptySubtitleLabel: NSTextField!
    private var emptyCardsContainer: NSView!
    private var emptyCreateButton: NSButton!
    private var emptyCliTipLabel: NSTextField!

    #if ENABLE_VM_TELEMETRY
    // Runtime Performance Card
    private var vmMemoryUsageLabel: NSTextField!
    private var vmCPULoadLabel: NSTextField!
    private var vmProcessInfoLabel: NSTextField!
    #endif

    // Bottom Status Bar
    private var statusBarView: NSView!
    private var serviceStatusDot: NSTextField!
    private var serviceStatusLabel: NSTextField!
    #if ENABLE_VM_TELEMETRY
    private var hostTelemetryLabel: NSTextField!
    #endif
    private var serviceToggleButton: NSButton!

    private var selectedVMName: String?
    private var refreshTimer: Timer?

    convenience init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 920, height: 620),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Cube Dashboard"
        window.minSize = NSSize(width: 800, height: 560)
        window.center()
        window.isReleasedWhenClosed = false

        self.init(window: window)
        window.delegate = self
        setupUI()
    }

    override func showWindow(_ sender: Any?) {
        NSApp.setActivationPolicy(.regular)
        super.showWindow(sender)
        window?.makeKeyAndOrderFront(sender)
        NSApp.activate(ignoringOtherApps: true)

        reloadData()
        startTimer()
    }

    func windowWillClose(_ notification: Notification) {
        stopTimer()
        let hasOtherWindows = PreferencesWindowController.shared.window?.isVisible == true ||
                              (NewVMWindowController.shared?.window?.isVisible == true)
        if !hasOtherWindows {
            NSApp.setActivationPolicy(.accessory)
        }
    }

    private func startTimer() {
        refreshTimer?.invalidate()
        refreshTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            self?.reloadData(silent: true)
        }
    }

    private func stopTimer() {
        refreshTimer?.invalidate()
        refreshTimer = nil
    }

    func selectVM(name: String) {
        showWindow(nil)
        selectedVMName = name
        reloadData()
    }

    // MARK: - UI Construction

    private func setupUI() {
        guard let window = self.window else { return }

        let mainContainer = NSView(frame: window.contentView!.bounds)
        mainContainer.autoresizingMask = [.width, .height]
        window.contentView = mainContainer

        let statusHeight: CGFloat = 30

        // 1. Bottom Status Bar (spans window width, always visible)
        setupBottomStatusBar(height: statusHeight, in: mainContainer)

        // 2. Split view: Sidebar (left) + Detail (right), sitting above status bar
        splitView = NSSplitView(frame: NSRect(x: 0, y: statusHeight, width: mainContainer.bounds.width, height: mainContainer.bounds.height - statusHeight))
        splitView.isVertical = true
        splitView.dividerStyle = .thin
        splitView.autoresizingMask = [.width, .height]
        mainContainer.addSubview(splitView)

        // Sidebar Container
        sidebarView = NSView(frame: NSRect(x: 0, y: 0, width: 240, height: splitView.bounds.height))
        setupSidebar()
        splitView.addSubview(sidebarView)

        // Detail Container
        detailContainerView = NSView(frame: NSRect(x: 241, y: 0, width: splitView.bounds.width - 241, height: splitView.bounds.height))
        setupDetailArea()
        splitView.addSubview(detailContainerView)

        splitView.setPosition(240, ofDividerAt: 0)
    }

    private func setupBottomStatusBar(height: CGFloat, in parent: NSView) {
        statusBarView = NSView(frame: NSRect(x: 0, y: 0, width: parent.bounds.width, height: height))
        statusBarView.autoresizingMask = [.width, .maxYMargin]

        let sep = NSBox(frame: NSRect(x: 0, y: height - 1, width: parent.bounds.width, height: 1))
        sep.boxType = .separator
        sep.autoresizingMask = [.width, .minYMargin]
        statusBarView.addSubview(sep)

        serviceStatusDot = NSTextField(labelWithString: "●")
        serviceStatusDot.font = NSFont.boldSystemFont(ofSize: 11)
        serviceStatusDot.frame = NSRect(x: 14, y: 6, width: 14, height: 18)
        statusBarView.addSubview(serviceStatusDot)

        serviceStatusLabel = NSTextField(labelWithString: "Cube Network Service: Checking...")
        serviceStatusLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        serviceStatusLabel.textColor = .secondaryLabelColor
        #if ENABLE_VM_TELEMETRY
        serviceStatusLabel.frame = NSRect(x: 30, y: 6, width: 280, height: 18)
        #else
        serviceStatusLabel.frame = NSRect(x: 30, y: 6, width: parent.bounds.width - 245, height: 18)
        serviceStatusLabel.autoresizingMask = [.width]
        #endif
        serviceStatusLabel.lineBreakMode = .byTruncatingTail
        statusBarView.addSubview(serviceStatusLabel)

        #if ENABLE_VM_TELEMETRY
        hostTelemetryLabel = NSTextField(labelWithString: "Host: Calculating...")
        hostTelemetryLabel.font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .regular)
        hostTelemetryLabel.textColor = .secondaryLabelColor
        hostTelemetryLabel.frame = NSRect(x: 320, y: 6, width: 260, height: 18)
        statusBarView.addSubview(hostTelemetryLabel)
        #endif

        let openPrefsBtn = NSButton(title: "Preferences...", target: self, action: #selector(openPreferencesClicked))
        openPrefsBtn.bezelStyle = .rounded
        openPrefsBtn.controlSize = .small
        openPrefsBtn.font = NSFont.systemFont(ofSize: 11)
        openPrefsBtn.frame = NSRect(x: parent.bounds.width - 105, y: 4, width: 95, height: 22)
        openPrefsBtn.autoresizingMask = [.minXMargin]
        statusBarView.addSubview(openPrefsBtn)

        serviceToggleButton = NSButton(title: "Start Service", target: self, action: #selector(toggleServiceClicked))
        serviceToggleButton.bezelStyle = .rounded
        serviceToggleButton.controlSize = .small
        serviceToggleButton.font = NSFont.systemFont(ofSize: 11)
        serviceToggleButton.frame = NSRect(x: parent.bounds.width - 208, y: 4, width: 98, height: 22)
        serviceToggleButton.autoresizingMask = [.minXMargin]
        statusBarView.addSubview(serviceToggleButton)

        parent.addSubview(statusBarView)
    }

    private func setupSidebar() {
        sidebarView.autoresizingMask = [.height]
        let barHeight = sidebarView.bounds.height

        // Top Header
        let header = NSView(frame: NSRect(x: 0, y: barHeight - 42, width: 240, height: 42))
        header.autoresizingMask = [.width, .minYMargin]

        let titleLabel = NSTextField(labelWithString: "Virtual Machines")
        titleLabel.font = NSFont.boldSystemFont(ofSize: 13)
        titleLabel.frame = NSRect(x: 14, y: 12, width: 140, height: 20)
        header.addSubview(titleLabel)

        vmCountLabel = NSTextField(labelWithString: "0")
        vmCountLabel.font = NSFont.systemFont(ofSize: 11, weight: .semibold)
        vmCountLabel.textColor = .secondaryLabelColor
        vmCountLabel.alignment = .right
        vmCountLabel.frame = NSRect(x: 160, y: 13, width: 66, height: 18)
        header.addSubview(vmCountLabel)

        let sepTop = NSBox(frame: NSRect(x: 0, y: 0, width: 240, height: 1))
        sepTop.boxType = .separator
        sepTop.autoresizingMask = [.width]
        header.addSubview(sepTop)
        sidebarView.addSubview(header)

        // Bottom Action Bar
        let bottomBar = NSView(frame: NSRect(x: 0, y: 0, width: 240, height: 38))
        bottomBar.autoresizingMask = [.width, .maxYMargin]

        let sepBottom = NSBox(frame: NSRect(x: 0, y: 37, width: 240, height: 1))
        sepBottom.boxType = .separator
        sepBottom.autoresizingMask = [.width]
        bottomBar.addSubview(sepBottom)

        let addBtn = NSButton(title: "+ New VM", target: self, action: #selector(newVMClicked))
        addBtn.bezelStyle = .recessed
        addBtn.isBordered = false
        addBtn.font = NSFont.systemFont(ofSize: 12, weight: .medium)
        addBtn.frame = NSRect(x: 10, y: 6, width: 85, height: 24)
        bottomBar.addSubview(addBtn)

        let refreshBtn = NSButton(title: "↻ Refresh", target: self, action: #selector(refreshClicked))
        refreshBtn.bezelStyle = .recessed
        refreshBtn.isBordered = false
        refreshBtn.font = NSFont.systemFont(ofSize: 12)
        refreshBtn.frame = NSRect(x: 155, y: 6, width: 75, height: 24)
        bottomBar.addSubview(refreshBtn)
        sidebarView.addSubview(bottomBar)

        // Scrollable Table View (clean without scrollbars or horizontal scroll)
        let scrollView = NSScrollView(frame: NSRect(x: 0, y: 38, width: 240, height: max(100, barHeight - 80)))
        scrollView.autoresizingMask = [.width, .height]
        scrollView.hasVerticalScroller = false
        scrollView.hasHorizontalScroller = false
        scrollView.horizontalScrollElasticity = .none
        scrollView.borderType = .noBorder

        tableView = NSTableView(frame: scrollView.bounds)
        tableView.headerView = nil
        tableView.autoresizingMask = [.width, .height]
        tableView.rowHeight = 46
        tableView.backgroundColor = .controlBackgroundColor
        tableView.selectionHighlightStyle = .regular
        tableView.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        tableView.target = self
        tableView.doubleAction = #selector(tableDoubleClicked)

        let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("VMColumn"))
        col.width = scrollView.contentSize.width
        col.resizingMask = .autoresizingMask
        tableView.addTableColumn(col)

        tableView.dataSource = self
        tableView.delegate = self
        scrollView.documentView = tableView
        sidebarView.addSubview(scrollView)
    }

    private func setupDetailArea() {
        detailContainerView.autoresizingMask = [.width, .height]

        // 1. Empty & Welcome State View
        emptyStateView = NSView(frame: detailContainerView.bounds)
        emptyStateView.autoresizingMask = [.width, .height]

        let emptyContainer = DashboardFlippedView(frame: NSRect(x: 0, y: 0, width: 540, height: 380))
        emptyContainer.autoresizingMask = [.minXMargin, .maxXMargin, .minYMargin, .maxYMargin]

        let iconView = NSImageView(frame: NSRect(x: 244, y: 12, width: 52, height: 52))
        let config = NSImage.SymbolConfiguration(pointSize: 42, weight: .regular)
        if let img = NSImage(systemSymbolName: "macmini", accessibilityDescription: nil)?.withSymbolConfiguration(config) {
            iconView.image = img
            iconView.contentTintColor = .systemBlue
        }
        emptyContainer.addSubview(iconView)

        emptyTitleLabel = NSTextField(labelWithString: "Welcome to Cube")
        emptyTitleLabel.font = NSFont.boldSystemFont(ofSize: 20)
        emptyTitleLabel.alignment = .center
        emptyTitleLabel.frame = NSRect(x: 0, y: 72, width: 540, height: 26)
        emptyContainer.addSubview(emptyTitleLabel)

        emptySubtitleLabel = NSTextField(labelWithString: "Native Apple Silicon Virtualization for FreeBSD")
        emptySubtitleLabel.font = NSFont.systemFont(ofSize: 12.5)
        emptySubtitleLabel.textColor = .secondaryLabelColor
        emptySubtitleLabel.alignment = .center
        emptySubtitleLabel.frame = NSRect(x: 0, y: 102, width: 540, height: 18)
        emptyContainer.addSubview(emptySubtitleLabel)

        // 3 Feature Cards
        emptyCardsContainer = NSView(frame: NSRect(x: 0, y: 134, width: 540, height: 122))
        let card1 = createQuickStartCard(
            icon: "bolt.fill",
            iconColor: .systemOrange,
            title: "1-Click Setup",
            text: "Official FreeBSD releases auto-downloaded with automated ZFS install and root SSH.",
            frame: NSRect(x: 0, y: 0, width: 168, height: 122)
        )
        let card2 = createQuickStartCard(
            icon: "network",
            iconColor: .systemBlue,
            title: "Zero-Config NAT",
            text: "High-speed userland proxy with automatic DHCP and SSH forwarding.",
            frame: NSRect(x: 186, y: 0, width: 168, height: 122)
        )
        let card3 = createQuickStartCard(
            icon: "folder.badge.gearshape",
            iconColor: .systemPurple,
            title: "Host Sharing",
            text: "Mount host Mac directories directly into your guest using high-speed VirtIO-9P.",
            frame: NSRect(x: 372, y: 0, width: 168, height: 122)
        )
        emptyCardsContainer.addSubview(card1)
        emptyCardsContainer.addSubview(card2)
        emptyCardsContainer.addSubview(card3)
        emptyContainer.addSubview(emptyCardsContainer)

        emptyCreateButton = NSButton(title: "+ Create Virtual Machine...", target: self, action: #selector(newVMClicked))
        emptyCreateButton.bezelStyle = .rounded
        emptyCreateButton.font = NSFont.boldSystemFont(ofSize: 13)
        emptyCreateButton.frame = NSRect(x: 155, y: 272, width: 230, height: 34)
        emptyContainer.addSubview(emptyCreateButton)

        emptyCliTipLabel = NSTextField(labelWithString: "💡 Prefer the terminal? Manage VMs with 'cubectl list' & 'cubectl start'.")
        emptyCliTipLabel.font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .regular)
        emptyCliTipLabel.textColor = .secondaryLabelColor
        emptyCliTipLabel.alignment = .center
        emptyCliTipLabel.frame = NSRect(x: 0, y: 318, width: 540, height: 18)
        emptyContainer.addSubview(emptyCliTipLabel)

        emptyContainer.setFrameOrigin(NSPoint(
            x: max(0, (detailContainerView.bounds.width - 540) / 2),
            y: max(0, (detailContainerView.bounds.height - 380) / 2)
        ))
        emptyStateView.addSubview(emptyContainer)
        detailContainerView.addSubview(emptyStateView)

        // 2. Active VM Detail View
        detailView = DashboardFlippedView(frame: detailContainerView.bounds)
        detailView.autoresizingMask = [.width, .height]
        detailView.isHidden = true

        let margin: CGFloat = 28
        let contentWidth: CGFloat = 580
        var y: CGFloat = 24

        // VM Header Bar
        let iconBadge = NSImageView(frame: NSRect(x: margin, y: y + 2, width: 38, height: 38))
        let heroConfig = NSImage.SymbolConfiguration(pointSize: 26, weight: .regular)
        if let heroImg = NSImage(systemSymbolName: "server.rack", accessibilityDescription: nil)?.withSymbolConfiguration(heroConfig) {
            iconBadge.image = heroImg
            iconBadge.contentTintColor = CubeTokens.accent
        }
        detailView.addSubview(iconBadge)

        vmNameLabel = NSTextField(labelWithString: "VM Name")
        vmNameLabel.font = CubeTokens.heroTitleFont
        vmNameLabel.frame = NSRect(x: margin + 48, y: y, width: 320, height: 26)
        detailView.addSubview(vmNameLabel)

        statusBadge = NSTextField(frame: NSRect(x: margin + 48, y: y + 27, width: 92, height: 20))
        statusBadge.cell = VerticallyCenteredTextFieldCell(textCell: "STOPPED")
        statusBadge.font = CubeTokens.badgeFont
        statusBadge.isBezeled = false
        statusBadge.drawsBackground = false
        statusBadge.isEditable = false
        statusBadge.isSelectable = false
        statusBadge.textColor = CubeTokens.secondaryText
        statusBadge.alignment = .center
        statusBadge.wantsLayer = true
        statusBadge.layer?.cornerRadius = CubeTokens.badgeCornerRadius
        statusBadge.layer?.borderWidth = 1
        statusBadge.layer?.borderColor = CubeTokens.cardBorder.cgColor
        statusBadge.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        detailView.addSubview(statusBadge)
        y += 58

        // Toolbar Action Buttons
        startStopButton = NSButton(title: "Start VM", target: self, action: #selector(toggleVMClicked))
        startStopButton.bezelStyle = .rounded
        startStopButton.font = NSFont.systemFont(ofSize: 12, weight: .semibold)
        startStopButton.frame = NSRect(x: margin, y: y, width: 96, height: 30)
        detailView.addSubview(startStopButton)

        sshButton = NSButton(title: "SSH Terminal", target: self, action: #selector(sshClicked))
        sshButton.bezelStyle = .rounded
        sshButton.font = NSFont.systemFont(ofSize: 12)
        sshButton.frame = NSRect(x: margin + 102, y: y, width: 114, height: 30)
        detailView.addSubview(sshButton)

        vncButton = NSButton(title: "VNC Display", target: self, action: #selector(vncClicked))
        vncButton.bezelStyle = .rounded
        vncButton.font = NSFont.systemFont(ofSize: 12)
        vncButton.frame = NSRect(x: margin + 222, y: y, width: 108, height: 30)
        detailView.addSubview(vncButton)

        logsButton = NSButton(title: "VM Logs...", target: self, action: #selector(logsClicked))
        logsButton.bezelStyle = .rounded
        logsButton.font = NSFont.systemFont(ofSize: 12)
        logsButton.frame = NSRect(x: margin + 336, y: y, width: 94, height: 30)
        detailView.addSubview(logsButton)

        deleteButton = NSButton(title: "Delete...", target: self, action: #selector(deleteClicked))
        deleteButton.bezelStyle = .rounded
        deleteButton.font = NSFont.systemFont(ofSize: 12)
        deleteButton.frame = NSRect(x: margin + 436, y: y, width: 88, height: 30)
        detailView.addSubview(deleteButton)
        y += 44

        // Card 1: Operating System & Hardware Specifications
        let card1Height: CGFloat = 176
        let hwCard = createCard(frame: NSRect(x: margin, y: y, width: contentWidth, height: card1Height))
        detailView.addSubview(hwCard)

        let hwTitle = NSTextField(labelWithString: "OPERATING SYSTEM & HARDWARE")
        hwTitle.font = CubeTokens.sectionHeaderFont
        hwTitle.textColor = CubeTokens.secondaryText
        hwTitle.frame = NSRect(x: 18, y: 14, width: contentWidth - 36, height: 16)
        hwCard.addSubview(hwTitle)

        var cy: CGFloat = 36
        osVersionLabel = addPropertyRow(label: "Operating System:", value: "FreeBSD", in: hwCard, y: cy)
        cy += 22
        archLabel = addPropertyRow(label: "Architecture:", value: "ARM64 (Apple Silicon Hypervisor.framework)", in: hwCard, y: cy)
        cy += 22
        cpusLabel = addPropertyRow(label: "vCPUs:", value: "2 Cores", in: hwCard, y: cy)
        cy += 22
        ramLabel = addPropertyRow(label: "Memory (RAM):", value: "2048 MB", in: hwCard, y: cy)
        cy += 22
        bootModeLabel = addPropertyRow(label: "Boot Mode:", value: "UEFI (QEMU_EFI.fd)", in: hwCard, y: cy)
        cy += 22

        diskPathLabel = addPropertyRow(label: "Virtual Disk:", value: "rootfs.img", in: hwCard, y: cy, valueWidth: contentWidth - 145 - 110)
        let revealDiskBtn = NSButton(title: "Reveal Disk", target: self, action: #selector(revealDiskClicked))
        revealDiskBtn.bezelStyle = .rounded
        revealDiskBtn.controlSize = .small
        revealDiskBtn.font = NSFont.systemFont(ofSize: 11)
        revealDiskBtn.frame = NSRect(x: contentWidth - 18 - 88, y: cy - 2, width: 88, height: 21)
        hwCard.addSubview(revealDiskBtn)

        y += card1Height + 14

        // Card 2: Networking & Connectivity
        let card2Height: CGFloat = 154
        let netCard = createCard(frame: NSRect(x: margin, y: y, width: contentWidth, height: card2Height))
        detailView.addSubview(netCard)

        let netTitle = NSTextField(labelWithString: "NETWORKING & REMOTE ACCESS")
        netTitle.font = CubeTokens.sectionHeaderFont
        netTitle.textColor = CubeTokens.secondaryText
        netTitle.frame = NSRect(x: 18, y: 14, width: contentWidth - 36, height: 16)
        netCard.addSubview(netTitle)

        cy = 36
        netModeLabel = addPropertyRow(label: "Network Mode:", value: "vmnet-proxy (Shared NAT)", in: netCard, y: cy)
        cy += 22

        ipLabel = addPropertyRow(label: "Guest IPv4:", value: "Detecting...", in: netCard, y: cy, valueWidth: contentWidth - 145 - 80)
        let copyIPBtn = NSButton(title: "Copy", target: self, action: #selector(copyIPClicked(_:)))
        copyIPBtn.bezelStyle = .rounded
        copyIPBtn.controlSize = .small
        copyIPBtn.font = NSFont.systemFont(ofSize: 11)
        copyIPBtn.frame = NSRect(x: contentWidth - 18 - 62, y: cy - 2, width: 62, height: 21)
        netCard.addSubview(copyIPBtn)
        cy += 22

        sshCommandLabel = addPropertyRow(label: "SSH Command:", value: "ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p 2222 root@127.0.0.1", in: netCard, y: cy, valueWidth: contentWidth - 145 - 80)
        sshCommandLabel.lineBreakMode = .byTruncatingMiddle
        let copySSHBtn = NSButton(title: "Copy", target: self, action: #selector(copySSHClicked(_:)))
        copySSHBtn.bezelStyle = .rounded
        copySSHBtn.controlSize = .small
        copySSHBtn.font = NSFont.systemFont(ofSize: 11)
        copySSHBtn.frame = NSRect(x: contentWidth - 18 - 62, y: cy - 2, width: 62, height: 21)
        netCard.addSubview(copySSHBtn)
        cy += 22

        vncLabel = addPropertyRow(label: "VNC Console:", value: "127.0.0.1:5901", in: netCard, y: cy)
        cy += 22

        sharesListLabel = addPropertyRow(label: "Shared Folders:", value: "None configured", in: netCard, y: cy, valueWidth: contentWidth - 145 - 198)

        copyMountCmdButton = NSButton(title: "Copy Mount", target: self, action: #selector(copyMountCmdClicked))
        copyMountCmdButton.bezelStyle = .rounded
        copyMountCmdButton.controlSize = .small
        copyMountCmdButton.font = NSFont.systemFont(ofSize: 11)
        copyMountCmdButton.frame = NSRect(x: contentWidth - 18 - 186, y: cy - 2, width: 88, height: 21)
        copyMountCmdButton.isHidden = true
        netCard.addSubview(copyMountCmdButton)

        manageSharesButton = NSButton(title: "+ Add Folder...", target: self, action: #selector(manageSharesClicked))
        manageSharesButton.bezelStyle = .rounded
        manageSharesButton.controlSize = .small
        manageSharesButton.font = NSFont.systemFont(ofSize: 11)
        manageSharesButton.frame = NSRect(x: contentWidth - 18 - 92, y: cy - 2, width: 92, height: 21)
        netCard.addSubview(manageSharesButton)

        y += card2Height + 14

        #if ENABLE_VM_TELEMETRY
        let card3Height: CGFloat = 114
        let telemetryCard = createCard(frame: NSRect(x: margin, y: y, width: contentWidth, height: card3Height))
        detailView.addSubview(telemetryCard)

        let telemetryTitle = NSTextField(labelWithString: "REAL-TIME PERFORMANCE & TELEMETRY")
        telemetryTitle.font = CubeTokens.sectionHeaderFont
        telemetryTitle.textColor = CubeTokens.secondaryText
        telemetryTitle.frame = NSRect(x: 18, y: 14, width: contentWidth - 36, height: 16)
        telemetryCard.addSubview(telemetryTitle)

        cy = 36
        vmMemoryUsageLabel = addPropertyRow(label: "Memory Footprint:", value: "Offline", in: telemetryCard, y: cy)
        cy += 22
        vmCPULoadLabel = addPropertyRow(label: "vCPU Activity:", value: "Offline", in: telemetryCard, y: cy)
        cy += 22
        vmProcessInfoLabel = addPropertyRow(label: "Hypervisor Engine:", value: "Apple Silicon Hypervisor.framework (ARM64)", in: telemetryCard, y: cy)
        #endif

        detailContainerView.addSubview(detailView)
    }

    private func createCard(frame: NSRect) -> DashboardFlippedView {
        let card = DashboardFlippedView(frame: frame)
        card.wantsLayer = true
        card.layer?.cornerRadius = CubeTokens.cardCornerRadius
        card.layer?.borderWidth = CubeTokens.cardBorderWidth
        card.layer?.borderColor = CubeTokens.cardBorder.cgColor
        card.layer?.backgroundColor = CubeTokens.cardBackground.cgColor
        return card
    }

    private func createQuickStartCard(icon: String, iconColor: NSColor, title: String, text: String, frame: NSRect) -> NSView {
        let card = DashboardFlippedView(frame: frame)
        card.wantsLayer = true
        card.layer?.cornerRadius = CubeTokens.cardCornerRadius
        card.layer?.borderWidth = CubeTokens.cardBorderWidth
        card.layer?.borderColor = CubeTokens.cardBorder.cgColor
        card.layer?.backgroundColor = CubeTokens.cardBackground.cgColor

        let iconView = NSImageView(frame: NSRect(x: 14, y: 14, width: 20, height: 20))
        let cfg = NSImage.SymbolConfiguration(pointSize: 16, weight: .semibold)
        if let img = NSImage(systemSymbolName: icon, accessibilityDescription: nil)?.withSymbolConfiguration(cfg) {
            iconView.image = img
            iconView.contentTintColor = iconColor
        }
        card.addSubview(iconView)

        let titleLabel = NSTextField(labelWithString: title)
        titleLabel.font = NSFont.boldSystemFont(ofSize: 12.5)
        titleLabel.textColor = CubeTokens.primaryText
        titleLabel.frame = NSRect(x: 38, y: 15, width: frame.width - 48, height: 18)
        card.addSubview(titleLabel)

        let descLabel = NSTextField(wrappingLabelWithString: text)
        descLabel.font = NSFont.systemFont(ofSize: 11)
        descLabel.textColor = CubeTokens.secondaryText
        descLabel.frame = NSRect(x: 14, y: 40, width: frame.width - 28, height: frame.height - 48)
        card.addSubview(descLabel)

        return card
    }

    private func updateEmptyState(hasVMs: Bool) {
        if hasVMs {
            emptyTitleLabel.stringValue = "No Virtual Machine Selected"
            emptySubtitleLabel.stringValue = "Select an existing virtual machine from the sidebar to inspect its settings."
            emptyCardsContainer.isHidden = true
            emptyCliTipLabel.isHidden = true
            emptyCreateButton.title = "+ Create Another VM..."
        } else {
            emptyTitleLabel.stringValue = "Welcome to Cube"
            emptySubtitleLabel.stringValue = "Native Apple Silicon Virtualization for FreeBSD"
            emptyCardsContainer.isHidden = false
            emptyCliTipLabel.isHidden = false
            emptyCreateButton.title = "+ Create Virtual Machine..."
        }
    }

    private func addPropertyRow(label: String, value: String, in view: NSView, y: CGFloat, labelX: CGFloat = 18, valueX: CGFloat = 145, valueWidth: CGFloat = 340) -> NSTextField {
        let lbl = NSTextField(labelWithString: label)
        lbl.font = CubeTokens.bodyLabelFont
        lbl.textColor = CubeTokens.secondaryText
        lbl.frame = NSRect(x: labelX, y: y, width: valueX - labelX - 10, height: 18)
        view.addSubview(lbl)

        let val = NSTextField(labelWithString: value)
        val.font = CubeTokens.bodyValueFont
        val.textColor = CubeTokens.primaryText
        val.isSelectable = true
        val.lineBreakMode = .byTruncatingMiddle
        val.frame = NSRect(x: valueX, y: y, width: valueWidth, height: 18)
        view.addSubview(val)
        return val
    }

    // MARK: - Data Synchronization

    func reloadData(silent: Bool = false) {
        VMManager.shared.refreshVMs { [weak self] vms in
            guard let self = self else { return }
            self.vmCountLabel.stringValue = "\(vms.count)"
            self.tableView.reloadData()

            // Auto-select first if none selected
            if self.selectedVMName == nil, let first = vms.first {
                self.selectedVMName = first.name
            }

            if let selected = self.selectedVMName,
               let vm = vms.first(where: { $0.name == selected }) {
                self.emptyStateView.isHidden = true
                self.detailView.isHidden = false
                self.updateDetailView(with: vm)

                // Update table selection
                if let idx = vms.firstIndex(where: { $0.name == selected }) {
                    if self.tableView.selectedRow != idx {
                        self.tableView.selectRowIndexes(IndexSet(integer: idx), byExtendingSelection: false)
                    }
                }
            } else {
                self.emptyStateView.isHidden = false
                self.detailView.isHidden = true
                self.updateEmptyState(hasVMs: !vms.isEmpty)
            }

            self.updateServiceBanner()
        }
    }

    private func updateDetailView(with vm: VMInfo) {
        vmNameLabel.stringValue = vm.name
        let isRunning = vm.isRunning
        let badgeText: String
        let badgeColor: NSColor
        let bgColor: NSColor

        if isRunning {
            let pidStr = vm.pid.map { " (PID \($0))" } ?? ""
            badgeText = "● RUNNING\(pidStr)"
            badgeColor = CubeTokens.runningGreen
            bgColor = CubeTokens.runningGreen.withAlphaComponent(0.12)
            statusBadge.layer?.borderColor = CubeTokens.runningGreen.withAlphaComponent(0.4).cgColor
            startStopButton.title = "Stop VM"
            sshButton.isEnabled = true
            vncButton.isEnabled = true
        } else {
            badgeText = "○ STOPPED"
            badgeColor = CubeTokens.stoppedGray
            bgColor = CubeTokens.subtleTagBg
            statusBadge.layer?.borderColor = CubeTokens.cardBorder.cgColor
            startStopButton.title = "Start VM"
            sshButton.isEnabled = true
            vncButton.isEnabled = true
        }

        statusBadge.stringValue = badgeText
        statusBadge.textColor = badgeColor
        statusBadge.layer?.backgroundColor = bgColor.cgColor

        let font = statusBadge.font ?? CubeTokens.badgeFont
        let textWidth = (badgeText as NSString).size(withAttributes: [.font: font]).width
        let badgeWidth = max(90, ceil(textWidth) + 18)
        statusBadge.frame = NSRect(x: 28 + 48, y: 24 + 27, width: badgeWidth, height: 20)

        let cfg = vm.loadConfig()
        let os = vm.os ?? cfg.os ?? "FreeBSD"
        osVersionLabel.stringValue = "\(os) (ARM64)"
        let memMode = cfg.memPrealloc ? "Pre-allocated" : "On-Demand"
        ramLabel.stringValue = "\(cfg.ramMB) MB (\(memMode))"
        bootModeLabel.stringValue = cfg.bootMode.uppercased()

        if let dp = cfg.diskPath {
            diskPathLabel.stringValue = (dp as NSString).lastPathComponent
        } else {
            diskPathLabel.stringValue = "rootfs.img"
        }

        netModeLabel.stringValue = cfg.netMode
        ipLabel.stringValue = vm.ip ?? (isRunning ? "Acquiring DHCP..." : "Offline")

        let port = vm.ssh_port ?? cfg.sshPort ?? 2222
        let host = (vm.ssh_port != nil || cfg.sshPort != nil) ? "127.0.0.1" : (vm.ip ?? "127.0.0.1")
        let fullCmd = "ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p \(port) root@\(host)"
        sshCommandLabel.stringValue = fullCmd
        sshCommandLabel.toolTip = fullCmd

        if let vnc = vm.vnc ?? cfg.vnc {
            vncLabel.stringValue = vnc
        } else {
            vncLabel.stringValue = "None (Headless)"
            if isRunning {
                vncButton.isEnabled = false
            }
        }

        if let first = cfg.shares.first {
            let mode = first.read_only ? "RO" : "RW"
            let shortPath = first.path.replacingOccurrences(of: NSHomeDirectory(), with: "~")
            sharesListLabel.stringValue = "\(first.tag) ➔ \(shortPath) (\(mode))"
            sharesListLabel.toolTip = "\(first.tag) -> \(first.path) (\(first.read_only ? "Read-Only" : "Read/Write"))"
            copyMountCmdButton.isHidden = false
            manageSharesButton.title = "Remove"
            manageSharesButton.isEnabled = !isRunning
        } else {
            sharesListLabel.stringValue = "None configured"
            sharesListLabel.toolTip = nil
            copyMountCmdButton.isHidden = true
            manageSharesButton.title = "+ Add Folder..."
            manageSharesButton.isEnabled = !isRunning
        }

        #if ENABLE_VM_TELEMETRY
        if isRunning, let pid = vm.pid, let stats = SystemMetrics.shared.sampleProcess(pid: pid_t(pid)) {
            let allocatedMB = cfg.ramMB
            let pct = allocatedMB > 0 ? (stats.residentMemoryMB / Double(allocatedMB)) * 100.0 : 0
            vmMemoryUsageLabel.stringValue = String(format: "%.1f MB footprint (of %d MB allocated, %.1f%%)", stats.residentMemoryMB, allocatedMB, pct)
            vmCPULoadLabel.stringValue = String(format: "%.1f%% vCPU (%d active threads)", stats.cpuPercent, stats.threadCount)
            vmProcessInfoLabel.stringValue = "PID \(pid) • Apple Silicon Hypervisor.framework (ARM64)"
        } else if isRunning {
            let pidStr = vm.pid.map { "PID \($0) • " } ?? ""
            vmMemoryUsageLabel.stringValue = "Active (measuring resident pages...)"
            vmCPULoadLabel.stringValue = "Active"
            vmProcessInfoLabel.stringValue = "\(pidStr)Apple Silicon Hypervisor.framework (ARM64)"
        } else {
            vmMemoryUsageLabel.stringValue = "Offline (0 MB RAM)"
            vmCPULoadLabel.stringValue = "Offline (0% CPU)"
            vmProcessInfoLabel.stringValue = "Stopped"
        }
        #endif
    }

    private func updateServiceBanner() {
        let isRunning = ServiceManager.shared.isRunning
        if isRunning {
            let mode = (ServiceManager.shared.activeMode ?? ServiceManager.shared.currentMode).description
            serviceStatusDot.stringValue = "●"
            serviceStatusDot.textColor = .systemGreen
            let text = "Cube Network Service: Active (\(mode))"
            serviceStatusLabel.stringValue = text
            serviceStatusLabel.toolTip = text
            serviceStatusLabel.textColor = .labelColor
            serviceToggleButton.title = "Stop Service"
        } else {
            serviceStatusDot.stringValue = "○"
            serviceStatusDot.textColor = .secondaryLabelColor
            let text = "Cube Network Service: Stopped"
            serviceStatusLabel.stringValue = text
            serviceStatusLabel.toolTip = text
            serviceStatusLabel.textColor = .secondaryLabelColor
            serviceToggleButton.title = "Start Service"
        }
        serviceToggleButton.isEnabled = true
        #if ENABLE_VM_TELEMETRY
        let hostStats = SystemMetrics.shared.sampleHost()
        hostTelemetryLabel.stringValue = "Host: \(hostStats.formattedCPU) CPU  •  \(hostStats.formattedMemory) RAM"
        #endif
    }

    @objc private func toggleServiceClicked() {
        serviceToggleButton.isEnabled = false
        if ServiceManager.shared.isRunning {
            ServiceManager.shared.stopService { [weak self] success, err in
                DispatchQueue.main.async {
                    self?.updateServiceBanner()
                    if !success, let e = err {
                        self?.showError("Failed to Stop Service", e)
                    }
                }
            }
        } else {
            ServiceManager.shared.startService { [weak self] success, err in
                DispatchQueue.main.async {
                    self?.updateServiceBanner()
                    if !success, let e = err {
                        self?.showError("Failed to Start Service", e)
                    }
                }
            }
        }
    }

    // MARK: - Table View DataSource & Delegate

    func numberOfRows(in tableView: NSTableView) -> Int {
        return VMManager.shared.vms.count
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let vms = VMManager.shared.vms
        guard row < vms.count else { return nil }
        let vm = vms[row]

        let cellId = NSUserInterfaceItemIdentifier("VMCell")
        var cell = tableView.makeView(withIdentifier: cellId, owner: nil)
        if cell == nil {
            let v = NSView(frame: NSRect(x: 0, y: 0, width: 240, height: 46))
            v.identifier = cellId
            v.autoresizingMask = [.width]

            let dot = NSTextField(labelWithString: "●")
            dot.tag = 101
            dot.frame = NSRect(x: 12, y: 15, width: 14, height: 18)
            v.addSubview(dot)

            let title = NSTextField(labelWithString: "")
            title.tag = 102
            title.font = CubeTokens.sidebarTitleFont
            title.lineBreakMode = .byTruncatingTail
            title.frame = NSRect(x: 28, y: 23, width: 200, height: 18)
            title.autoresizingMask = [.width]
            v.addSubview(title)

            let subtitle = NSTextField(labelWithString: "")
            subtitle.tag = 103
            subtitle.font = CubeTokens.sidebarSubtitleFont
            subtitle.textColor = CubeTokens.secondaryText
            subtitle.lineBreakMode = .byTruncatingTail
            subtitle.frame = NSRect(x: 28, y: 6, width: 200, height: 15)
            subtitle.autoresizingMask = [.width]
            v.addSubview(subtitle)

            cell = v
        }

        if let dot = cell?.viewWithTag(101) as? NSTextField {
            dot.stringValue = vm.isRunning ? "●" : "○"
            dot.textColor = vm.isRunning ? CubeTokens.runningGreen : CubeTokens.stoppedGray
        }
        if let title = cell?.viewWithTag(102) as? NSTextField {
            title.stringValue = vm.name
        }
        if let subtitle = cell?.viewWithTag(103) as? NSTextField {
            let os = vm.os ?? "FreeBSD"
            if vm.isRunning {
                #if ENABLE_VM_TELEMETRY
                if let pid = vm.pid, let stats = SystemMetrics.shared.sampleProcess(pid: pid_t(pid)) {
                    subtitle.stringValue = "\(os) • \(stats.formattedMemory) • \(stats.formattedCPU)"
                } else if let ip = vm.ip {
                    subtitle.stringValue = "\(os) • \(ip)"
                } else {
                    subtitle.stringValue = "\(os) • Running"
                }
                #else
                if let ip = vm.ip {
                    subtitle.stringValue = "\(os) • \(ip)"
                } else {
                    subtitle.stringValue = "\(os) • Running"
                }
                #endif
            } else {
                subtitle.stringValue = "\(os) • Stopped"
            }
        }
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        let row = tableView.selectedRow
        let vms = VMManager.shared.vms
        if row >= 0 && row < vms.count {
            selectedVMName = vms[row].name
            updateDetailView(with: vms[row])
            emptyStateView.isHidden = true
            detailView.isHidden = false
        }
    }

    @objc private func tableDoubleClicked() {
        let row = tableView.selectedRow
        let vms = VMManager.shared.vms
        guard row >= 0 && row < vms.count else { return }
        let vm = vms[row]
        if vm.isRunning {
            VMManager.shared.openTerminalSSH(for: vm)
        } else {
            toggleVMClicked()
        }
    }

    // MARK: - Actions

    @objc private func newVMClicked() {
        NewVMWindowController.show()
    }

    @objc private func refreshClicked() {
        reloadData()
    }

    @objc private func toggleVMClicked() {
        guard let name = selectedVMName,
              let vm = VMManager.shared.vms.first(where: { $0.name == name }) else { return }

        if vm.isRunning {
            VMManager.shared.stopVM(name: name) { [weak self] success, err in
                if !success, let e = err {
                    self?.showError("Failed to Stop VM", e)
                }
                self?.reloadData()
            }
        } else {
            VMManager.shared.startVM(name: name) { [weak self] success, err in
                if !success, let e = err {
                    self?.showError("Failed to Start VM", e)
                }
                self?.reloadData()
            }
        }
    }

    @objc private func sshClicked() {
        guard let name = selectedVMName,
              let vm = VMManager.shared.vms.first(where: { $0.name == name }) else { return }
        VMManager.shared.openTerminalSSH(for: vm)
    }

    @objc private func vncClicked() {
        guard let name = selectedVMName,
              let vm = VMManager.shared.vms.first(where: { $0.name == name }) else { return }
        VMManager.shared.openVNC(for: vm)
    }

    @objc private func logsClicked() {
        guard let name = selectedVMName else { return }
        VMLogWindowController.show(for: name)
    }

    @objc private func deleteClicked() {
        guard let name = selectedVMName else { return }
        let alert = NSAlert()
        alert.messageText = "Delete Virtual Machine '\(name)'?"
        alert.informativeText = "This will permanently remove the VM directory and its virtual disk image. This action cannot be undone."
        alert.alertStyle = .critical
        alert.addButton(withTitle: "Delete")
        alert.addButton(withTitle: "Cancel")
        if alert.runModal() == .alertFirstButtonReturn {
            VMManager.shared.deleteVM(name: name) { [weak self] success, err in
                if !success, let e = err {
                    self?.showError("Failed to Delete VM", e)
                }
                self?.selectedVMName = nil
                self?.reloadData()
            }
        }
    }

    @objc private func revealDiskClicked() {
        guard let name = selectedVMName,
              let vm = VMManager.shared.vms.first(where: { $0.name == name }) else { return }
        let cfg = vm.loadConfig()
        let diskPath = cfg.diskPath ?? "\(VMManager.shared.vmsDirectory)/\(name)/rootfs.img"
        if FileManager.default.fileExists(atPath: diskPath) {
            NSWorkspace.shared.selectFile(diskPath, inFileViewerRootedAtPath: (diskPath as NSString).deletingLastPathComponent)
        } else {
            let dir = "\(VMManager.shared.vmsDirectory)/\(name)"
            NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: dir)
        }
    }

    @objc private func copyIPClicked(_ sender: Any?) {
        guard let name = selectedVMName,
              let vm = VMManager.shared.vms.first(where: { $0.name == name }),
              let ip = vm.ip else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(ip, forType: .string)

        if let btn = sender as? NSButton {
            btn.title = "Copied!"
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                btn.title = "Copy"
            }
        }
    }

    @objc private func copySSHClicked(_ sender: Any?) {
        guard let name = selectedVMName,
              let vm = VMManager.shared.vms.first(where: { $0.name == name }) else { return }
        let cfg = vm.loadConfig()
        let port = vm.ssh_port ?? cfg.sshPort ?? 2222
        let host = (vm.ssh_port != nil || cfg.sshPort != nil) ? "127.0.0.1" : (vm.ip ?? "127.0.0.1")
        let cmd = "ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p \(port) root@\(host)"
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(cmd, forType: .string)

        if let btn = sender as? NSButton {
            btn.title = "Copied!"
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                btn.title = "Copy"
            }
        }
    }

    @objc private func openPreferencesClicked() {
        PreferencesWindowController.shared.showTab("network")
    }

    @objc private func copyMountCmdClicked() {
        guard let name = selectedVMName,
              let vm = VMManager.shared.vms.first(where: { $0.name == name }) else { return }
        let cfg = vm.loadConfig()
        guard let share = cfg.shares.first else { return }

        let isLinux = (vm.os ?? cfg.os ?? "").lowercased().contains("linux") ||
                      (vm.os ?? cfg.os ?? "").lowercased().contains("ubuntu") ||
                      (vm.os ?? cfg.os ?? "").lowercased().contains("debian")
        let cmd = isLinux ? "mount -t 9p -o trans=virtio,version=9p2000.L \(share.tag) /mnt/host" : "mount -t p9fs \(share.tag) /mnt/host"

        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(cmd, forType: .string)

        let orig = copyMountCmdButton.title
        copyMountCmdButton.title = "Copied ✓"
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [weak self] in
            self?.copyMountCmdButton.title = orig
        }
    }

    @objc private func manageSharesClicked() {
        guard let name = selectedVMName,
              let vm = VMManager.shared.vms.first(where: { $0.name == name }) else { return }
        if vm.isRunning {
            showError("Cannot Modify Shared Folders", "Please stop '\(name)' before modifying shared folder configurations.")
            return
        }

        let cfg = vm.loadConfig()
        if let existing = cfg.shares.first {
            let alert = NSAlert()
            alert.messageText = "Manage Shared Folder"
            let mode = existing.read_only ? "Read-Only" : "Read/Write"
            alert.informativeText = "Current export tag: '\(existing.tag)'\nPath: \(existing.path)\nMode: \(mode)"
            alert.alertStyle = .informational
            alert.addButton(withTitle: "Remove Share")
            alert.addButton(withTitle: "Change Folder...")
            alert.addButton(withTitle: "Cancel")

            let response = alert.runModal()
            if response == .alertFirstButtonReturn {
                VMManager.shared.updateShares(for: name, shares: []) { [weak self] success, err in
                    if success {
                        self?.reloadData()
                    } else if let e = err {
                        self?.showError("Failed to Remove Share", e)
                    }
                }
            } else if response == .alertSecondButtonReturn {
                promptAddShare(for: name, defaultTag: existing.tag)
            }
        } else {
            promptAddShare(for: name, defaultTag: nil)
        }
    }

    private func promptAddShare(for vmName: String, defaultTag: String?) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Choose Folder"
        panel.message = "Select a host directory to share with '\(vmName)'"

        let accessory = NSView(frame: NSRect(x: 0, y: 0, width: 320, height: 56))
        let tagLabel = NSTextField(labelWithString: "Mount Tag:")
        tagLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        tagLabel.frame = NSRect(x: 0, y: 30, width: 70, height: 18)
        accessory.addSubview(tagLabel)

        let tagField = NSTextField(frame: NSRect(x: 75, y: 28, width: 235, height: 22))
        tagField.placeholderString = defaultTag ?? "e.g. host-share (default: folder name)"
        if let defaultTag = defaultTag {
            tagField.stringValue = defaultTag
        }
        accessory.addSubview(tagField)

        let roBox = NSButton(checkboxWithTitle: "Read-Only access inside guest VM", target: nil, action: nil)
        roBox.font = NSFont.systemFont(ofSize: 11)
        roBox.frame = NSRect(x: 75, y: 4, width: 235, height: 20)
        accessory.addSubview(roBox)

        panel.accessoryView = accessory

        if panel.runModal() == .OK, let url = panel.url {
            var tag = tagField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            if tag.isEmpty {
                tag = url.lastPathComponent.lowercased().replacingOccurrences(of: " ", with: "-")
            }
            let isReadOnly = (roBox.state == .on)
            let newShare = VMShareConfig(tag: tag, path: url.path, read_only: isReadOnly)
            VMManager.shared.updateShares(for: vmName, shares: [newShare]) { [weak self] success, err in
                if success {
                    self?.reloadData()
                } else if let e = err {
                    self?.showError("Failed to Add Shared Folder", e)
                }
            }
        }
    }

    private func showError(_ title: String, _ message: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.runModal()
    }
}
