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

    // Bottom Status Bar
    private var statusBarView: NSView!
    private var serviceStatusDot: NSTextField!
    private var serviceStatusLabel: NSTextField!
    private var serviceToggleButton: NSButton!

    private var selectedVMName: String?
    private var refreshTimer: Timer?

    convenience init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 880, height: 580),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Cube Dashboard"
        window.minSize = NSSize(width: 760, height: 480)
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
        serviceStatusLabel.frame = NSRect(x: 30, y: 6, width: 480, height: 18)
        serviceStatusLabel.autoresizingMask = [.width]
        statusBarView.addSubview(serviceStatusLabel)

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

        // Scrollable Table View
        let scrollView = NSScrollView(frame: NSRect(x: 0, y: 38, width: 240, height: max(100, barHeight - 80)))
        scrollView.autoresizingMask = [.width, .height]
        scrollView.hasVerticalScroller = true
        scrollView.borderType = .noBorder

        tableView = NSTableView(frame: scrollView.bounds)
        tableView.headerView = nil
        tableView.autoresizingMask = [.width, .height]
        tableView.rowHeight = 44
        tableView.backgroundColor = .controlBackgroundColor
        tableView.selectionHighlightStyle = .regular
        tableView.target = self
        tableView.doubleAction = #selector(tableDoubleClicked)

        let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("VMColumn"))
        col.width = 240
        tableView.addTableColumn(col)

        tableView.dataSource = self
        tableView.delegate = self
        scrollView.documentView = tableView
        sidebarView.addSubview(scrollView)
    }

    private func setupDetailArea() {
        detailContainerView.autoresizingMask = [.width, .height]

        // 1. Empty State View
        emptyStateView = NSView(frame: detailContainerView.bounds)
        emptyStateView.autoresizingMask = [.width, .height]

        let emptyContainer = NSView(frame: NSRect(x: 0, y: 0, width: 340, height: 220))
        emptyContainer.autoresizingMask = []

        let iconView = NSImageView(frame: NSRect(x: 130, y: 130, width: 80, height: 80))
        let config = NSImage.SymbolConfiguration(pointSize: 52, weight: .light)
        if let img = NSImage(systemSymbolName: "macmini", accessibilityDescription: nil)?.withSymbolConfiguration(config) {
            iconView.image = img
            iconView.contentTintColor = .tertiaryLabelColor
        }
        emptyContainer.addSubview(iconView)

        let emptyTitle = NSTextField(labelWithString: "No Virtual Machine Selected")
        emptyTitle.font = NSFont.boldSystemFont(ofSize: 15)
        emptyTitle.alignment = .center
        emptyTitle.frame = NSRect(x: 0, y: 95, width: 340, height: 22)
        emptyContainer.addSubview(emptyTitle)

        let emptyDesc = NSTextField(labelWithString: "Select an existing virtual machine from the sidebar\nor create a new FreeBSD / ARM64 guest.")
        emptyDesc.font = NSFont.systemFont(ofSize: 12)
        emptyDesc.textColor = .secondaryLabelColor
        emptyDesc.alignment = .center
        emptyDesc.frame = NSRect(x: 0, y: 55, width: 340, height: 32)
        emptyContainer.addSubview(emptyDesc)

        let createBtn = NSButton(title: "+ Create Virtual Machine...", target: self, action: #selector(newVMClicked))
        createBtn.bezelStyle = .rounded
        createBtn.frame = NSRect(x: 70, y: 10, width: 200, height: 32)
        emptyContainer.addSubview(createBtn)

        emptyContainer.setFrameOrigin(NSPoint(
            x: max(0, (detailContainerView.bounds.width - 340) / 2),
            y: max(0, (detailContainerView.bounds.height - 220) / 2)
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
            iconBadge.contentTintColor = .systemBlue
        }
        detailView.addSubview(iconBadge)

        vmNameLabel = NSTextField(labelWithString: "VM Name")
        vmNameLabel.font = NSFont.boldSystemFont(ofSize: 20)
        vmNameLabel.frame = NSRect(x: margin + 48, y: y, width: 320, height: 26)
        detailView.addSubview(vmNameLabel)

        statusBadge = NSTextField(frame: NSRect(x: margin + 48, y: y + 26, width: 100, height: 20))
        statusBadge.cell = VerticallyCenteredTextFieldCell(textCell: "STOPPED")
        statusBadge.font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .semibold)
        statusBadge.isBezeled = false
        statusBadge.drawsBackground = false
        statusBadge.isEditable = false
        statusBadge.isSelectable = false
        statusBadge.textColor = .secondaryLabelColor
        statusBadge.alignment = .center
        statusBadge.wantsLayer = true
        statusBadge.layer?.cornerRadius = 5
        statusBadge.layer?.borderWidth = 1
        statusBadge.layer?.borderColor = NSColor.separatorColor.cgColor
        statusBadge.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        detailView.addSubview(statusBadge)
        y += 56

        // Toolbar Action Buttons
        startStopButton = NSButton(title: "Start VM", target: self, action: #selector(toggleVMClicked))
        startStopButton.bezelStyle = .rounded
        startStopButton.frame = NSRect(x: margin, y: y, width: 100, height: 30)
        detailView.addSubview(startStopButton)

        sshButton = NSButton(title: "SSH Terminal", target: self, action: #selector(sshClicked))
        sshButton.bezelStyle = .rounded
        sshButton.frame = NSRect(x: margin + 106, y: y, width: 115, height: 30)
        detailView.addSubview(sshButton)

        vncButton = NSButton(title: "VNC Display", target: self, action: #selector(vncClicked))
        vncButton.bezelStyle = .rounded
        vncButton.frame = NSRect(x: margin + 227, y: y, width: 110, height: 30)
        detailView.addSubview(vncButton)

        logsButton = NSButton(title: "VM Logs...", target: self, action: #selector(logsClicked))
        logsButton.bezelStyle = .rounded
        logsButton.frame = NSRect(x: margin + 343, y: y, width: 95, height: 30)
        detailView.addSubview(logsButton)

        deleteButton = NSButton(title: "Delete...", target: self, action: #selector(deleteClicked))
        deleteButton.bezelStyle = .rounded
        deleteButton.frame = NSRect(x: margin + 444, y: y, width: 90, height: 30)
        detailView.addSubview(deleteButton)
        y += 42

        // Separator
        let sep1 = NSBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 1))
        sep1.boxType = .separator
        detailView.addSubview(sep1)
        y += 18

        // Card 1: Operating System & Hardware Specifications
        let hwTitle = NSTextField(labelWithString: "Operating System & Hardware")
        hwTitle.font = NSFont.boldSystemFont(ofSize: 13)
        hwTitle.frame = NSRect(x: margin, y: y, width: contentWidth, height: 18)
        detailView.addSubview(hwTitle)
        y += 24

        osVersionLabel = addPropertyRow(label: "Operating System:", value: "FreeBSD", in: detailView, y: y)
        y += 22
        archLabel = addPropertyRow(label: "Architecture:", value: "ARM64 (Apple Silicon Hypervisor.framework)", in: detailView, y: y)
        y += 22
        cpusLabel = addPropertyRow(label: "vCPUs:", value: "2 Cores", in: detailView, y: y)
        y += 22
        ramLabel = addPropertyRow(label: "Memory (RAM):", value: "2048 MB", in: detailView, y: y)
        y += 22
        bootModeLabel = addPropertyRow(label: "Boot Mode:", value: "UEFI (QEMU_EFI.fd)", in: detailView, y: y)
        y += 22

        diskPathLabel = addPropertyRow(label: "Virtual Disk:", value: "rootfs.img", in: detailView, y: y)
        let revealDiskBtn = NSButton(title: "Reveal Disk", target: self, action: #selector(revealDiskClicked))
        revealDiskBtn.bezelStyle = .rounded
        revealDiskBtn.controlSize = .small
        revealDiskBtn.font = NSFont.systemFont(ofSize: 11)
        revealDiskBtn.frame = NSRect(x: margin + contentWidth - 92, y: y - 3, width: 92, height: 21)
        detailView.addSubview(revealDiskBtn)
        y += 30

        // Separator 2
        let sep2 = NSBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 1))
        sep2.boxType = .separator
        detailView.addSubview(sep2)
        y += 18

        // Card 2: Networking & Connectivity
        let netTitle = NSTextField(labelWithString: "Networking & Remote Access")
        netTitle.font = NSFont.boldSystemFont(ofSize: 13)
        netTitle.frame = NSRect(x: margin, y: y, width: contentWidth, height: 18)
        detailView.addSubview(netTitle)
        y += 24

        netModeLabel = addPropertyRow(label: "Network Mode:", value: "vmnet-proxy (Shared NAT)", in: detailView, y: y)
        y += 22

        ipLabel = addPropertyRow(label: "Guest IPv4:", value: "Detecting...", in: detailView, y: y)
        let copyIPBtn = NSButton(title: "Copy", target: self, action: #selector(copyIPClicked(_:)))
        copyIPBtn.bezelStyle = .rounded
        copyIPBtn.controlSize = .small
        copyIPBtn.font = NSFont.systemFont(ofSize: 11)
        copyIPBtn.frame = NSRect(x: margin + contentWidth - 66, y: y - 3, width: 66, height: 21)
        detailView.addSubview(copyIPBtn)
        y += 22

        sshCommandLabel = addPropertyRow(label: "SSH Command:", value: "ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p 2222 root@127.0.0.1", in: detailView, y: y)
        sshCommandLabel.lineBreakMode = .byTruncatingMiddle
        let copySSHBtn = NSButton(title: "Copy", target: self, action: #selector(copySSHClicked(_:)))
        copySSHBtn.bezelStyle = .rounded
        copySSHBtn.controlSize = .small
        copySSHBtn.font = NSFont.systemFont(ofSize: 11)
        copySSHBtn.frame = NSRect(x: margin + contentWidth - 66, y: y - 3, width: 66, height: 21)
        detailView.addSubview(copySSHBtn)
        y += 22

        vncLabel = addPropertyRow(label: "VNC Console:", value: "127.0.0.1:5901", in: detailView, y: y)
        y += 24

        detailContainerView.addSubview(detailView)
    }

    private func addPropertyRow(label: String, value: String, in view: NSView, y: CGFloat) -> NSTextField {
        let margin: CGFloat = 28
        let lbl = NSTextField(labelWithString: label)
        lbl.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        lbl.textColor = .secondaryLabelColor
        lbl.frame = NSRect(x: margin, y: y, width: 110, height: 18)
        view.addSubview(lbl)

        let val = NSTextField(labelWithString: value)
        val.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        val.textColor = .labelColor
        val.isSelectable = true
        val.lineBreakMode = .byTruncatingMiddle
        val.frame = NSRect(x: margin + 115, y: y, width: 395, height: 18)
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
            badgeColor = .systemGreen
            bgColor = NSColor.systemGreen.withAlphaComponent(0.08)
            statusBadge.layer?.borderColor = NSColor.systemGreen.withAlphaComponent(0.4).cgColor
            startStopButton.title = "Stop VM"
            sshButton.isEnabled = true
            vncButton.isEnabled = true
        } else {
            badgeText = "○ STOPPED"
            badgeColor = .secondaryLabelColor
            bgColor = NSColor.windowBackgroundColor
            statusBadge.layer?.borderColor = NSColor.separatorColor.cgColor
            startStopButton.title = "Start VM"
            sshButton.isEnabled = true
            vncButton.isEnabled = true
        }

        statusBadge.stringValue = badgeText
        statusBadge.textColor = badgeColor
        statusBadge.layer?.backgroundColor = bgColor.cgColor

        let font = statusBadge.font ?? NSFont.monospacedSystemFont(ofSize: 10.5, weight: .semibold)
        let textWidth = (badgeText as NSString).size(withAttributes: [.font: font]).width
        let badgeWidth = max(95, ceil(textWidth) + 20)
        statusBadge.frame = NSRect(x: 28 + 48, y: 24 + 26, width: badgeWidth, height: 20)

        let cfg = vm.loadConfig()
        let os = vm.os ?? cfg.os ?? "FreeBSD"
        osVersionLabel.stringValue = "\(os) (ARM64)"
        cpusLabel.stringValue = "\(cfg.cpus) vCPUs"
        ramLabel.stringValue = "\(cfg.ramMB) MB"
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
    }

    private func updateServiceBanner() {
        let isRunning = ServiceManager.shared.isRunning
        if isRunning {
            let mode = (ServiceManager.shared.activeMode ?? ServiceManager.shared.currentMode).description
            serviceStatusDot.stringValue = "●"
            serviceStatusDot.textColor = .systemGreen
            serviceStatusLabel.stringValue = "Cube Network Service: Active (\(mode))"
            serviceStatusLabel.textColor = .labelColor
            serviceToggleButton.title = "Stop Service"
        } else {
            serviceStatusDot.stringValue = "○"
            serviceStatusDot.textColor = .secondaryLabelColor
            serviceStatusLabel.stringValue = "Cube Network Service: Stopped"
            serviceStatusLabel.textColor = .secondaryLabelColor
            serviceToggleButton.title = "Start Service"
        }
        serviceToggleButton.isEnabled = true
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
            let v = NSView(frame: NSRect(x: 0, y: 0, width: 240, height: 44))
            v.identifier = cellId

            let dot = NSTextField(labelWithString: "●")
            dot.tag = 101
            dot.frame = NSRect(x: 12, y: 15, width: 14, height: 18)
            v.addSubview(dot)

            let title = NSTextField(labelWithString: "")
            title.tag = 102
            title.font = NSFont.systemFont(ofSize: 13, weight: .semibold)
            title.frame = NSRect(x: 28, y: 22, width: 195, height: 18)
            v.addSubview(title)

            let subtitle = NSTextField(labelWithString: "")
            subtitle.tag = 103
            subtitle.font = NSFont.systemFont(ofSize: 10)
            subtitle.textColor = .secondaryLabelColor
            subtitle.frame = NSRect(x: 28, y: 5, width: 195, height: 15)
            v.addSubview(subtitle)

            cell = v
        }

        if let dot = cell?.viewWithTag(101) as? NSTextField {
            dot.stringValue = vm.isRunning ? "●" : "○"
            dot.textColor = vm.isRunning ? .systemGreen : .secondaryLabelColor
        }
        if let title = cell?.viewWithTag(102) as? NSTextField {
            title.stringValue = vm.name
        }
        if let subtitle = cell?.viewWithTag(103) as? NSTextField {
            let os = vm.os ?? "FreeBSD"
            if vm.isRunning {
                subtitle.stringValue = "\(os) • Running"
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

    private func showError(_ title: String, _ message: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.runModal()
    }
}
