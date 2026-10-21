import Foundation
import AppKit

private class DashboardFlippedView: NSView {
    override var isFlipped: Bool { return true }
}

private class DashboardCardView: DashboardFlippedView {
    init() {
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        wantsLayer = true
        layer?.cornerRadius = CubeTokens.cardCornerRadius
        layer?.borderWidth = CubeTokens.cardBorderWidth
        layer?.borderColor = CubeTokens.cardBorder.cgColor
        layer?.backgroundColor = CubeTokens.cardBackground.cgColor
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
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

// MARK: - Cube Design System Tokens (Compresto-Style Translucent Frosted Glass Aesthetic)
enum CubeTokens {
    // Accessibility Preferences
    static var reduceTransparency: Bool {
        return NSWorkspace.shared.accessibilityDisplayShouldReduceTransparency
    }
    static var reduceMotion: Bool {
        return NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
    }

    // Window Border Specular Highlight
    static var windowBorder: NSColor {
        if reduceTransparency {
            return NSColor.separatorColor
        }
        return NSColor(name: nil, dynamicProvider: { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(white: 1.0, alpha: 0.12)
                : NSColor(white: 1.0, alpha: 0.50)
        })
    }

    // Dynamic Translucent Surfaces (Compresto Liquid Frosted Glass)
    static var canvasBackground: NSColor {
        return .clear // Root NSVisualEffectView provides the wallpaper blur
    }

    static var statusBarBackground: NSColor {
        if reduceTransparency {
            return NSColor.windowBackgroundColor
        }
        return NSColor(name: nil, dynamicProvider: { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(white: 0.08, alpha: 0.45)
                : NSColor(white: 1.0, alpha: 0.35)
        })
    }

    // Layered Frosted Glass Cards (Floating Milk Glass in Light Mode, Smoked Obsidian in Dark Mode)
    static var cardBackground: NSColor {
        if reduceTransparency {
            return NSColor.controlBackgroundColor
        }
        return NSColor(name: nil, dynamicProvider: { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(red: 0.12, green: 0.14, blue: 0.17, alpha: 0.55) // Smoked Obsidian Glass
                : NSColor(white: 1.0, alpha: 0.46) // Pure Translucent Milk Glass
        })
    }

    // Specular White Edge (Light Catching Rim)
    static var cardBorder: NSColor {
        if reduceTransparency {
            return NSColor.separatorColor
        }
        return NSColor(name: nil, dynamicProvider: { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(white: 1.0, alpha: 0.14) // Subtle white rim in dark mode
                : NSColor(white: 1.0, alpha: 0.72) // Crisp white specular light catch
        })
    }

    // Hairline Separators
    static var separatorColor: NSColor {
        if reduceTransparency {
            return NSColor.separatorColor
        }
        return NSColor(name: nil, dynamicProvider: { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(white: 1.0, alpha: 0.08)
                : NSColor(white: 1.0, alpha: 0.40)
        })
    }

    // Frosted Capsule Pills & Badges
    static var capsuleBackground: NSColor {
        if reduceTransparency {
            return NSColor.controlColor
        }
        return NSColor(name: nil, dynamicProvider: { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(white: 0.20, alpha: 0.40)
                : NSColor(white: 1.0, alpha: 0.65)
        })
    }

    static var capsuleBorder: NSColor {
        if reduceTransparency {
            return NSColor.separatorColor
        }
        return NSColor(name: nil, dynamicProvider: { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                ? NSColor(white: 1.0, alpha: 0.15)
                : NSColor(white: 1.0, alpha: 0.85)
        })
    }

    // Accessible Animated Copy Feedback
    static func animateCopyFeedback(
        button: NSButton,
        temporaryTitle: String = "Copied!",
        temporaryIcon: String = "checkmark",
        originalTitle: String = "Copy",
        originalIcon: String = "doc.on.doc",
        duration: TimeInterval = 1.2
    ) {
        let updateToCopied = {
            button.title = temporaryTitle
            button.image = NSImage(systemSymbolName: temporaryIcon, accessibilityDescription: nil)
            button.imagePosition = .imageLeading
            button.imageHugsTitle = true
        }
        let restore = {
            button.title = originalTitle
            button.image = NSImage(systemSymbolName: originalIcon, accessibilityDescription: nil)
            button.imagePosition = .imageLeading
            button.imageHugsTitle = true
        }

        if reduceMotion {
            updateToCopied()
            DispatchQueue.main.asyncAfter(deadline: .now() + duration) {
                restore()
            }
        } else {
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.08
                button.animator().alphaValue = 0.5
            }, completionHandler: {
                updateToCopied()
                NSAnimationContext.runAnimationGroup({ ctx in
                    ctx.duration = 0.12
                    button.animator().alphaValue = 1.0
                })
            })
            DispatchQueue.main.asyncAfter(deadline: .now() + duration) {
                NSAnimationContext.runAnimationGroup({ ctx in
                    ctx.duration = 0.08
                    button.animator().alphaValue = 0.5
                }, completionHandler: {
                    restore()
                    NSAnimationContext.runAnimationGroup({ ctx in
                        ctx.duration = 0.12
                        button.animator().alphaValue = 1.0
                    })
                })
            }
        }
    }

    // Accents & State Colors
    static let accent = NSColor(red: 0.0, green: 0.48, blue: 1.0, alpha: 1.0)
    static let runningGreen = NSColor(red: 0.19, green: 0.82, blue: 0.44, alpha: 1.0) // Emerald Glow
    static let stoppedGray = NSColor.secondaryLabelColor
    static let warningAmber = NSColor(red: 1.0, green: 0.62, blue: 0.04, alpha: 1.0)
    static let destructiveRed = NSColor(red: 1.0, green: 0.27, blue: 0.23, alpha: 1.0)

    static let primaryText = NSColor.labelColor
    static let secondaryText = NSColor.secondaryLabelColor
    static let tertiaryText = NSColor.tertiaryLabelColor
    static let subtleTagBg = NSColor.quaternaryLabelColor.withAlphaComponent(0.08)

    // Typography (Refined SF Pro & SF Mono Tabular)
    static let heroTitleFont = NSFont.systemFont(ofSize: 22, weight: .bold)
    static let sectionHeaderFont = NSFont.systemFont(ofSize: 10.5, weight: .bold)
    static let bodyLabelFont = NSFont.systemFont(ofSize: 11.5, weight: .medium)
    static let bodyValueFont = NSFont.monospacedSystemFont(ofSize: 11.5, weight: .regular)
    static let badgeFont = NSFont.monospacedSystemFont(ofSize: 10, weight: .semibold)
    static let sidebarTitleFont = NSFont.systemFont(ofSize: 13, weight: .semibold)
    static let sidebarSubtitleFont = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .regular)

    // Layout Metrics & Radii
    static let cardCornerRadius: CGFloat = 16.0 // Compresto rounded cards
    static let badgeCornerRadius: CGFloat = 10.0 // Capsule style
    static let cardBorderWidth: CGFloat = 1.0 // Crisp specular rim
}

class DashboardWindowController: NSWindowController, NSWindowDelegate, NSTableViewDataSource, NSTableViewDelegate, NSSplitViewDelegate {
    static let shared = DashboardWindowController()

    // Navigation Architecture (Sidebar + Detail Split View)
    private var splitView: NSSplitView!
    private var sidebarView: NSVisualEffectView!
    private var detailContainerView: NSView!

    // Sidebar components
    private var tableView: NSTableView!
    private var vmCountLabel: NSTextField!

    // Detail components - Empty State
    private var emptyStateView: NSView!
    private var emptyTitleLabel: NSTextField!
    private var emptySubtitleLabel: NSTextField!
    private var emptyCardsContainer: NSView!
    private var emptyCreateButton: NSButton!
    private var emptyCliTipLabel: NSTextField!

    // Detail components - Active VM
    private var detailView: NSScrollView!
    private var vmNameLabel: NSTextField!
    private var statusBadge: NSTextField!
    private var statusBadgeWidthConstraint: NSLayoutConstraint?
    private var startStopButton: NSButton!
    private var sshButton: NSButton!
    private var vncButton: NSButton!
    private var logsButton: NSButton!
    private var commandsButton: NSButton!
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

    private var rootVisualEffect: NSVisualEffectView!
    private var selectedVMName: String?
    private var refreshTimer: Timer?

    deinit {
        NSWorkspace.shared.notificationCenter.removeObserver(self)
    }

    convenience init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 960, height: 580),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Cube Dashboard"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isOpaque = false
        window.backgroundColor = .clear
        window.minSize = NSSize(width: 820, height: 480)
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

        // Root Liquid Translucent Glass (underWindowBackground blur behind window)
        rootVisualEffect = NSVisualEffectView(frame: window.contentView?.bounds ?? NSRect(x: 0, y: 0, width: 960, height: 580))
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

        let statusHeight: CGFloat = 32

        // 1. Bottom Status Bar (spans window width, translucent glass)
        setupBottomStatusBar(height: statusHeight, in: rootVisualEffect)

        // 2. Split view: Sidebar (left) + Detail (right), sitting above status bar
        splitView = NSSplitView()
        splitView.translatesAutoresizingMaskIntoConstraints = false
        splitView.isVertical = true
        splitView.dividerStyle = .thin
        splitView.delegate = self
        rootVisualEffect.addSubview(splitView)

        // Sidebar Container (Translucent frosted sidebar effect)
        sidebarView = NSVisualEffectView()
        sidebarView.translatesAutoresizingMaskIntoConstraints = false
        sidebarView.material = .sidebar
        sidebarView.blendingMode = .behindWindow
        sidebarView.state = .active
        setupSidebar()
        splitView.addSubview(sidebarView)

        // Detail Container (Transparent so frosted wallpaper shines through)
        detailContainerView = NSView()
        detailContainerView.translatesAutoresizingMaskIntoConstraints = false
        detailContainerView.wantsLayer = true
        detailContainerView.layer?.backgroundColor = NSColor.clear.cgColor
        setupDetailArea()
        splitView.addSubview(detailContainerView)

        let sidebarWidthConstraint = sidebarView.widthAnchor.constraint(equalToConstant: 240)
        sidebarWidthConstraint.priority = NSLayoutConstraint.Priority(499)

        NSLayoutConstraint.activate([
            // Status bar pinned to bottom
            statusBarView.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            statusBarView.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            statusBarView.bottomAnchor.constraint(equalTo: rootVisualEffect.bottomAnchor),
            statusBarView.heightAnchor.constraint(equalToConstant: statusHeight),

            // Split view pinned above status bar
            splitView.topAnchor.constraint(equalTo: rootVisualEffect.topAnchor),
            splitView.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            splitView.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            splitView.bottomAnchor.constraint(equalTo: statusBarView.topAnchor),

            // Sidebar & Detail sizing rules
            sidebarView.widthAnchor.constraint(greaterThanOrEqualToConstant: 200),
            sidebarView.widthAnchor.constraint(lessThanOrEqualToConstant: 340),
            sidebarWidthConstraint,
            detailContainerView.widthAnchor.constraint(greaterThanOrEqualToConstant: 500)
        ])

        splitView.setPosition(240, ofDividerAt: 0)

        // Accessibility Listeners
        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(accessibilityDisplayOptionsDidChange),
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification,
            object: nil
        )
        updateAccessibilityPreferences()
    }

    private func setupBottomStatusBar(height: CGFloat, in parent: NSView) {
        statusBarView = NSView()
        statusBarView.translatesAutoresizingMaskIntoConstraints = false
        statusBarView.wantsLayer = true
        statusBarView.layer?.backgroundColor = CubeTokens.statusBarBackground.cgColor

        let sep = NSBox()
        sep.translatesAutoresizingMaskIntoConstraints = false
        sep.boxType = .custom
        sep.borderWidth = 0
        sep.fillColor = CubeTokens.separatorColor
        statusBarView.addSubview(sep)

        serviceStatusDot = NSTextField(labelWithString: "●")
        serviceStatusDot.translatesAutoresizingMaskIntoConstraints = false
        serviceStatusDot.font = NSFont.boldSystemFont(ofSize: 10)
        statusBarView.addSubview(serviceStatusDot)

        serviceStatusLabel = NSTextField(labelWithString: "Cube Network Service: Checking...")
        serviceStatusLabel.translatesAutoresizingMaskIntoConstraints = false
        serviceStatusLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        serviceStatusLabel.textColor = CubeTokens.secondaryText
        serviceStatusLabel.lineBreakMode = .byTruncatingTail
        statusBarView.addSubview(serviceStatusLabel)

        let openPrefsBtn = NSButton(title: "Preferences...", target: self, action: #selector(openPreferencesClicked))
        openPrefsBtn.translatesAutoresizingMaskIntoConstraints = false
        openPrefsBtn.bezelStyle = .rounded
        openPrefsBtn.controlSize = .small
        openPrefsBtn.font = NSFont.systemFont(ofSize: 11)
        statusBarView.addSubview(openPrefsBtn)

        serviceToggleButton = NSButton(title: "Start Service", target: self, action: #selector(toggleServiceClicked))
        serviceToggleButton.translatesAutoresizingMaskIntoConstraints = false
        serviceToggleButton.bezelStyle = .rounded
        serviceToggleButton.controlSize = .small
        serviceToggleButton.font = NSFont.systemFont(ofSize: 11)
        statusBarView.addSubview(serviceToggleButton)

        var statusConstraints: [NSLayoutConstraint] = [
            sep.topAnchor.constraint(equalTo: statusBarView.topAnchor),
            sep.leadingAnchor.constraint(equalTo: statusBarView.leadingAnchor),
            sep.trailingAnchor.constraint(equalTo: statusBarView.trailingAnchor),
            sep.heightAnchor.constraint(equalToConstant: 1),

            serviceStatusDot.leadingAnchor.constraint(equalTo: statusBarView.leadingAnchor, constant: 14),
            serviceStatusDot.centerYAnchor.constraint(equalTo: statusBarView.centerYAnchor),
            serviceStatusDot.widthAnchor.constraint(equalToConstant: 14),

            serviceStatusLabel.leadingAnchor.constraint(equalTo: serviceStatusDot.trailingAnchor, constant: 4),
            serviceStatusLabel.centerYAnchor.constraint(equalTo: statusBarView.centerYAnchor),

            openPrefsBtn.trailingAnchor.constraint(equalTo: statusBarView.trailingAnchor, constant: -14),
            openPrefsBtn.centerYAnchor.constraint(equalTo: statusBarView.centerYAnchor),
            openPrefsBtn.heightAnchor.constraint(equalToConstant: 22),

            serviceToggleButton.trailingAnchor.constraint(equalTo: openPrefsBtn.leadingAnchor, constant: -8),
            serviceToggleButton.centerYAnchor.constraint(equalTo: statusBarView.centerYAnchor),
            serviceToggleButton.heightAnchor.constraint(equalToConstant: 22)
        ]

        #if ENABLE_VM_TELEMETRY
        hostTelemetryLabel = NSTextField(labelWithString: "Host: Calculating...")
        hostTelemetryLabel.translatesAutoresizingMaskIntoConstraints = false
        hostTelemetryLabel.font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .regular)
        hostTelemetryLabel.textColor = CubeTokens.secondaryText
        statusBarView.addSubview(hostTelemetryLabel)

        statusConstraints.append(contentsOf: [
            hostTelemetryLabel.leadingAnchor.constraint(equalTo: serviceStatusLabel.trailingAnchor, constant: 14),
            hostTelemetryLabel.centerYAnchor.constraint(equalTo: statusBarView.centerYAnchor),
            hostTelemetryLabel.trailingAnchor.constraint(lessThanOrEqualTo: serviceToggleButton.leadingAnchor, constant: -12)
        ])
        #else
        statusConstraints.append(
            serviceStatusLabel.trailingAnchor.constraint(lessThanOrEqualTo: serviceToggleButton.leadingAnchor, constant: -12)
        )
        #endif

        NSLayoutConstraint.activate(statusConstraints)
        parent.addSubview(statusBarView)
    }

    private func setupSidebar() {
        let headerHeight: CGFloat = 86

        // Top Header - 86pt height to cleanly clear native window traffic light buttons at top-left
        let header = NSView()
        header.translatesAutoresizingMaskIntoConstraints = false

        let appIconView = NSImageView()
        appIconView.translatesAutoresizingMaskIntoConstraints = false
        appIconView.imageScaling = .scaleProportionallyUpOrDown
        if let appIcon = NSApp.applicationIconImage {
            appIconView.image = appIcon
        } else {
            let cfg = NSImage.SymbolConfiguration(pointSize: 26, weight: .regular)
            if let fallback = NSImage(systemSymbolName: "cube.fill", accessibilityDescription: nil)?.withSymbolConfiguration(cfg) {
                appIconView.image = fallback
                appIconView.contentTintColor = CubeTokens.accent
            }
        }
        header.addSubview(appIconView)

        let titleLabel = NSTextField(labelWithString: "Virtual Machines")
        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.font = NSFont.boldSystemFont(ofSize: 13.5)
        titleLabel.textColor = CubeTokens.primaryText
        header.addSubview(titleLabel)

        vmCountLabel = NSTextField(labelWithString: "0")
        vmCountLabel.translatesAutoresizingMaskIntoConstraints = false
        vmCountLabel.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .semibold)
        vmCountLabel.textColor = CubeTokens.secondaryText
        vmCountLabel.alignment = .right
        header.addSubview(vmCountLabel)

        let sepTop = NSBox()
        sepTop.translatesAutoresizingMaskIntoConstraints = false
        sepTop.boxType = .custom
        sepTop.borderWidth = 0
        sepTop.fillColor = CubeTokens.separatorColor
        header.addSubview(sepTop)
        sidebarView.addSubview(header)

        // Bottom Action Bar
        let bottomBar = NSView()
        bottomBar.translatesAutoresizingMaskIntoConstraints = false

        let sepBottom = NSBox()
        sepBottom.translatesAutoresizingMaskIntoConstraints = false
        sepBottom.boxType = .custom
        sepBottom.borderWidth = 0
        sepBottom.fillColor = CubeTokens.separatorColor
        bottomBar.addSubview(sepBottom)

        let addBtn = NSButton(title: "+ New VM", target: self, action: #selector(newVMClicked))
        addBtn.translatesAutoresizingMaskIntoConstraints = false
        addBtn.bezelStyle = .recessed
        addBtn.isBordered = false
        addBtn.font = NSFont.systemFont(ofSize: 12, weight: .medium)
        bottomBar.addSubview(addBtn)

        let refreshBtn = NSButton(title: "↻ Refresh", target: self, action: #selector(refreshClicked))
        refreshBtn.translatesAutoresizingMaskIntoConstraints = false
        refreshBtn.bezelStyle = .recessed
        refreshBtn.isBordered = false
        refreshBtn.font = NSFont.systemFont(ofSize: 12, weight: .medium)
        bottomBar.addSubview(refreshBtn)

        sidebarView.addSubview(bottomBar)

        // Middle Table View
        let scrollView = NSScrollView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.hasVerticalScroller = true
        scrollView.drawsBackground = false

        tableView = NSTableView()
        tableView.headerView = nil
        tableView.backgroundColor = .clear
        tableView.rowHeight = 48
        tableView.intercellSpacing = NSSize(width: 0, height: 4)
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

        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: sidebarView.topAnchor),
            header.leadingAnchor.constraint(equalTo: sidebarView.leadingAnchor),
            header.trailingAnchor.constraint(equalTo: sidebarView.trailingAnchor),
            header.heightAnchor.constraint(equalToConstant: headerHeight),

            appIconView.leadingAnchor.constraint(equalTo: header.leadingAnchor, constant: 18),
            appIconView.bottomAnchor.constraint(equalTo: header.bottomAnchor, constant: -10),
            appIconView.widthAnchor.constraint(equalToConstant: 36),
            appIconView.heightAnchor.constraint(equalToConstant: 36),

            titleLabel.leadingAnchor.constraint(equalTo: appIconView.trailingAnchor, constant: 10),
            titleLabel.centerYAnchor.constraint(equalTo: appIconView.centerYAnchor),

            vmCountLabel.trailingAnchor.constraint(equalTo: header.trailingAnchor, constant: -18),
            vmCountLabel.centerYAnchor.constraint(equalTo: appIconView.centerYAnchor),
            vmCountLabel.leadingAnchor.constraint(greaterThanOrEqualTo: titleLabel.trailingAnchor, constant: 8),

            sepTop.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            sepTop.trailingAnchor.constraint(equalTo: header.trailingAnchor),
            sepTop.bottomAnchor.constraint(equalTo: header.bottomAnchor),
            sepTop.heightAnchor.constraint(equalToConstant: 1),

            bottomBar.bottomAnchor.constraint(equalTo: sidebarView.bottomAnchor),
            bottomBar.leadingAnchor.constraint(equalTo: sidebarView.leadingAnchor),
            bottomBar.trailingAnchor.constraint(equalTo: sidebarView.trailingAnchor),
            bottomBar.heightAnchor.constraint(equalToConstant: 38),

            sepBottom.topAnchor.constraint(equalTo: bottomBar.topAnchor),
            sepBottom.leadingAnchor.constraint(equalTo: bottomBar.leadingAnchor),
            sepBottom.trailingAnchor.constraint(equalTo: bottomBar.trailingAnchor),
            sepBottom.heightAnchor.constraint(equalToConstant: 1),

            addBtn.leadingAnchor.constraint(equalTo: bottomBar.leadingAnchor, constant: 10),
            addBtn.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            addBtn.heightAnchor.constraint(equalToConstant: 24),

            refreshBtn.trailingAnchor.constraint(equalTo: bottomBar.trailingAnchor, constant: -10),
            refreshBtn.centerYAnchor.constraint(equalTo: bottomBar.centerYAnchor),
            refreshBtn.leadingAnchor.constraint(greaterThanOrEqualTo: addBtn.trailingAnchor, constant: 8),
            refreshBtn.heightAnchor.constraint(equalToConstant: 24),

            scrollView.topAnchor.constraint(equalTo: header.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: sidebarView.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: sidebarView.trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: bottomBar.topAnchor)
        ])
    }

    private func setupDetailArea() {
        // 1. Empty & Welcome State
        emptyStateView = NSView()
        emptyStateView.translatesAutoresizingMaskIntoConstraints = false

        let emptyContainer = NSView()
        emptyContainer.translatesAutoresizingMaskIntoConstraints = false

        let iconView = NSImageView()
        iconView.translatesAutoresizingMaskIntoConstraints = false
        let config = NSImage.SymbolConfiguration(pointSize: 42, weight: .regular)
        if let img = NSImage(systemSymbolName: "macmini", accessibilityDescription: nil)?.withSymbolConfiguration(config) {
            iconView.image = img
            iconView.contentTintColor = CubeTokens.accent
        }
        emptyContainer.addSubview(iconView)

        emptyTitleLabel = NSTextField(labelWithString: "Welcome to Cube")
        emptyTitleLabel.translatesAutoresizingMaskIntoConstraints = false
        emptyTitleLabel.font = NSFont.systemFont(ofSize: 22, weight: .bold)
        emptyTitleLabel.alignment = .center
        emptyTitleLabel.textColor = CubeTokens.primaryText
        emptyContainer.addSubview(emptyTitleLabel)

        emptySubtitleLabel = NSTextField(labelWithString: "Native Apple Silicon Virtualization for FreeBSD")
        emptySubtitleLabel.translatesAutoresizingMaskIntoConstraints = false
        emptySubtitleLabel.font = NSFont.systemFont(ofSize: 13)
        emptySubtitleLabel.textColor = CubeTokens.secondaryText
        emptySubtitleLabel.alignment = .center
        emptyContainer.addSubview(emptySubtitleLabel)

        // 3 Feature Cards (Compresto Frosted Glass)
        emptyCardsContainer = NSView()
        emptyCardsContainer.translatesAutoresizingMaskIntoConstraints = false

        let card1 = createQuickStartCard(
            icon: "bolt.fill",
            iconColor: .systemOrange,
            title: "1-Click Setup",
            text: "Official FreeBSD releases auto-downloaded with automated ZFS install and root SSH."
        )
        let card2 = createQuickStartCard(
            icon: "network",
            iconColor: CubeTokens.accent,
            title: "Zero-Config NAT",
            text: "High-speed userland proxy with automatic DHCP and SSH forwarding."
        )
        let card3 = createQuickStartCard(
            icon: "folder.badge.gearshape",
            iconColor: .systemPurple,
            title: "Host Sharing",
            text: "Mount host Mac directories directly into your guest using high-speed VirtIO-9P."
        )

        let cardsStack = NSStackView(views: [card1, card2, card3])
        cardsStack.translatesAutoresizingMaskIntoConstraints = false
        cardsStack.orientation = .horizontal
        cardsStack.distribution = .fillEqually
        cardsStack.spacing = 16
        emptyCardsContainer.addSubview(cardsStack)
        emptyContainer.addSubview(emptyCardsContainer)

        emptyCreateButton = NSButton(title: "+ Create Virtual Machine...", target: self, action: #selector(newVMClicked))
        emptyCreateButton.translatesAutoresizingMaskIntoConstraints = false
        emptyCreateButton.bezelStyle = .rounded
        emptyCreateButton.font = NSFont.boldSystemFont(ofSize: 13)
        emptyContainer.addSubview(emptyCreateButton)

        emptyCliTipLabel = NSTextField(labelWithString: "💡 Prefer the terminal? Manage VMs with 'cubectl list' & 'cubectl start'.")
        emptyCliTipLabel.translatesAutoresizingMaskIntoConstraints = false
        emptyCliTipLabel.font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .regular)
        emptyCliTipLabel.textColor = CubeTokens.secondaryText
        emptyCliTipLabel.alignment = .center
        emptyContainer.addSubview(emptyCliTipLabel)

        emptyStateView.addSubview(emptyContainer)
        detailContainerView.addSubview(emptyStateView)

        NSLayoutConstraint.activate([
            emptyStateView.topAnchor.constraint(equalTo: detailContainerView.topAnchor),
            emptyStateView.leadingAnchor.constraint(equalTo: detailContainerView.leadingAnchor),
            emptyStateView.trailingAnchor.constraint(equalTo: detailContainerView.trailingAnchor),
            emptyStateView.bottomAnchor.constraint(equalTo: detailContainerView.bottomAnchor),

            emptyContainer.centerXAnchor.constraint(equalTo: emptyStateView.centerXAnchor),
            emptyContainer.centerYAnchor.constraint(equalTo: emptyStateView.centerYAnchor),
            emptyContainer.widthAnchor.constraint(equalToConstant: 560),

            iconView.centerXAnchor.constraint(equalTo: emptyContainer.centerXAnchor),
            iconView.topAnchor.constraint(equalTo: emptyContainer.topAnchor, constant: 16),
            iconView.widthAnchor.constraint(equalToConstant: 52),
            iconView.heightAnchor.constraint(equalToConstant: 52),

            emptyTitleLabel.topAnchor.constraint(equalTo: iconView.bottomAnchor, constant: 8),
            emptyTitleLabel.leadingAnchor.constraint(equalTo: emptyContainer.leadingAnchor),
            emptyTitleLabel.trailingAnchor.constraint(equalTo: emptyContainer.trailingAnchor),

            emptySubtitleLabel.topAnchor.constraint(equalTo: emptyTitleLabel.bottomAnchor, constant: 4),
            emptySubtitleLabel.leadingAnchor.constraint(equalTo: emptyContainer.leadingAnchor),
            emptySubtitleLabel.trailingAnchor.constraint(equalTo: emptyContainer.trailingAnchor),

            emptyCardsContainer.topAnchor.constraint(equalTo: emptySubtitleLabel.bottomAnchor, constant: 14),
            emptyCardsContainer.leadingAnchor.constraint(equalTo: emptyContainer.leadingAnchor),
            emptyCardsContainer.trailingAnchor.constraint(equalTo: emptyContainer.trailingAnchor),
            emptyCardsContainer.heightAnchor.constraint(equalToConstant: 124),

            cardsStack.topAnchor.constraint(equalTo: emptyCardsContainer.topAnchor),
            cardsStack.leadingAnchor.constraint(equalTo: emptyCardsContainer.leadingAnchor),
            cardsStack.trailingAnchor.constraint(equalTo: emptyCardsContainer.trailingAnchor),
            cardsStack.bottomAnchor.constraint(equalTo: emptyCardsContainer.bottomAnchor),

            emptyCreateButton.topAnchor.constraint(equalTo: emptyCardsContainer.bottomAnchor, constant: 16),
            emptyCreateButton.centerXAnchor.constraint(equalTo: emptyContainer.centerXAnchor),
            emptyCreateButton.widthAnchor.constraint(equalToConstant: 230),
            emptyCreateButton.heightAnchor.constraint(equalToConstant: 34),

            emptyCliTipLabel.topAnchor.constraint(equalTo: emptyCreateButton.bottomAnchor, constant: 12),
            emptyCliTipLabel.leadingAnchor.constraint(equalTo: emptyContainer.leadingAnchor),
            emptyCliTipLabel.trailingAnchor.constraint(equalTo: emptyContainer.trailingAnchor),
            emptyCliTipLabel.bottomAnchor.constraint(equalTo: emptyContainer.bottomAnchor, constant: -10)
        ])

        // 2. Active VM Detail View (Embedded in NSScrollView for adaptive window resizing)
        detailView = NSScrollView()
        detailView.translatesAutoresizingMaskIntoConstraints = false
        detailView.hasVerticalScroller = true
        detailView.autohidesScrollers = true
        detailView.borderType = .noBorder
        detailView.drawsBackground = false
        detailView.automaticallyAdjustsContentInsets = false
        detailView.isHidden = true

        let detailContentView = DashboardFlippedView()
        detailContentView.translatesAutoresizingMaskIntoConstraints = false
        detailView.documentView = detailContentView
        detailContainerView.addSubview(detailView)

        // VM Header Bar
        let iconBadge = NSImageView()
        iconBadge.translatesAutoresizingMaskIntoConstraints = false
        let heroConfig = NSImage.SymbolConfiguration(pointSize: 26, weight: .regular)
        if let heroImg = NSImage(systemSymbolName: "server.rack", accessibilityDescription: nil)?.withSymbolConfiguration(heroConfig) {
            iconBadge.image = heroImg
            iconBadge.contentTintColor = CubeTokens.accent
        }
        detailContentView.addSubview(iconBadge)

        vmNameLabel = NSTextField(labelWithString: "VM Name")
        vmNameLabel.translatesAutoresizingMaskIntoConstraints = false
        vmNameLabel.font = CubeTokens.heroTitleFont
        vmNameLabel.textColor = CubeTokens.primaryText
        detailContentView.addSubview(vmNameLabel)

        statusBadge = NSTextField()
        statusBadge.translatesAutoresizingMaskIntoConstraints = false
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
        statusBadge.layer?.borderColor = CubeTokens.capsuleBorder.cgColor
        statusBadge.layer?.backgroundColor = CubeTokens.capsuleBackground.cgColor
        detailContentView.addSubview(statusBadge)

        let btnSymbolConfig = NSImage.SymbolConfiguration(pointSize: 11.5, weight: .medium)

        // Toolbar Action Buttons
        startStopButton = NSButton(title: "Start VM", target: self, action: #selector(toggleVMClicked))
        startStopButton.translatesAutoresizingMaskIntoConstraints = false
        startStopButton.bezelStyle = .rounded
        startStopButton.image = NSImage(systemSymbolName: "play.fill", accessibilityDescription: nil)?.withSymbolConfiguration(btnSymbolConfig)
        startStopButton.imagePosition = .imageLeading
        startStopButton.imageHugsTitle = true
        startStopButton.font = NSFont.systemFont(ofSize: 12, weight: .semibold)

        sshButton = NSButton(title: "SSH Terminal", target: self, action: #selector(sshClicked))
        sshButton.translatesAutoresizingMaskIntoConstraints = false
        sshButton.bezelStyle = .rounded
        sshButton.image = NSImage(systemSymbolName: "terminal", accessibilityDescription: nil)?.withSymbolConfiguration(btnSymbolConfig)
        sshButton.imagePosition = .imageLeading
        sshButton.imageHugsTitle = true
        sshButton.font = NSFont.systemFont(ofSize: 12)

        vncButton = NSButton(title: "VNC Display", target: self, action: #selector(vncClicked))
        vncButton.translatesAutoresizingMaskIntoConstraints = false
        vncButton.bezelStyle = .rounded
        vncButton.image = NSImage(systemSymbolName: "display", accessibilityDescription: nil)?.withSymbolConfiguration(btnSymbolConfig)
        vncButton.imagePosition = .imageLeading
        vncButton.imageHugsTitle = true
        vncButton.font = NSFont.systemFont(ofSize: 12)

        logsButton = NSButton(title: "VM Logs...", target: self, action: #selector(logsClicked))
        logsButton.translatesAutoresizingMaskIntoConstraints = false
        logsButton.bezelStyle = .rounded
        logsButton.image = NSImage(systemSymbolName: "doc.text", accessibilityDescription: nil)?.withSymbolConfiguration(btnSymbolConfig)
        logsButton.imagePosition = .imageLeading
        logsButton.imageHugsTitle = true
        logsButton.font = NSFont.systemFont(ofSize: 12)

        commandsButton = NSButton(title: "Commands...", target: self, action: #selector(commandsClicked))
        commandsButton.translatesAutoresizingMaskIntoConstraints = false
        commandsButton.bezelStyle = .rounded
        commandsButton.image = NSImage(systemSymbolName: "command", accessibilityDescription: nil)?.withSymbolConfiguration(btnSymbolConfig)
        commandsButton.imagePosition = .imageLeading
        commandsButton.imageHugsTitle = true
        commandsButton.font = NSFont.systemFont(ofSize: 12)

        deleteButton = NSButton(title: "Delete...", target: self, action: #selector(deleteClicked))
        deleteButton.translatesAutoresizingMaskIntoConstraints = false
        deleteButton.bezelStyle = .rounded
        deleteButton.image = NSImage(systemSymbolName: "trash", accessibilityDescription: nil)?.withSymbolConfiguration(btnSymbolConfig)
        deleteButton.imagePosition = .imageLeading
        deleteButton.imageHugsTitle = true
        deleteButton.font = NSFont.systemFont(ofSize: 12)

        let toolbarStack = NSStackView(views: [startStopButton, sshButton, vncButton, logsButton, commandsButton, deleteButton])
        toolbarStack.translatesAutoresizingMaskIntoConstraints = false
        toolbarStack.orientation = .horizontal
        toolbarStack.spacing = 6
        detailContentView.addSubview(toolbarStack)

        // Card 1: Operating System & Hardware Specifications (Compresto Glass)
        let hwCard = createCard()
        detailContentView.addSubview(hwCard)

        let hwTitle = NSTextField(labelWithString: "OPERATING SYSTEM & HARDWARE")
        hwTitle.translatesAutoresizingMaskIntoConstraints = false
        hwTitle.font = CubeTokens.sectionHeaderFont
        hwTitle.textColor = CubeTokens.secondaryText
        hwCard.addSubview(hwTitle)

        let showDiskBtn = NSButton(title: "Show in Finder", target: self, action: #selector(revealDiskClicked))
        showDiskBtn.translatesAutoresizingMaskIntoConstraints = false
        showDiskBtn.bezelStyle = .rounded
        showDiskBtn.controlSize = .small
        showDiskBtn.font = NSFont.systemFont(ofSize: 11)
        hwCard.addSubview(showDiskBtn)

        let (o1, b1) = addPropertyRow(label: "Operating System:", value: "FreeBSD", in: hwCard, topAnchor: hwTitle.bottomAnchor, topOffset: 12)
        osVersionLabel = o1
        let (o2, b2) = addPropertyRow(label: "Architecture:", value: "ARM64 (Apple Silicon Hypervisor)", in: hwCard, topAnchor: b1, topOffset: 9)
        archLabel = o2
        let (o3, b3) = addPropertyRow(label: "vCPU Cores:", value: "4 vCPUs", in: hwCard, topAnchor: b2, topOffset: 9)
        cpusLabel = o3
        let (o4, b4) = addPropertyRow(label: "Memory (RAM):", value: "4096 MB", in: hwCard, topAnchor: b3, topOffset: 9)
        ramLabel = o4
        let (o5, b5) = addPropertyRow(label: "Boot Loader:", value: "Direct Kernel / EFI", in: hwCard, topAnchor: b4, topOffset: 9)
        bootModeLabel = o5
        let (o6, b6) = addPropertyRow(label: "Virtual Disk:", value: "~/Dev/vmm/vms/.../rootfs.img", in: hwCard, topAnchor: b5, topOffset: 9, trailingTarget: showDiskBtn.leadingAnchor, trailingOffset: -8)
        diskPathLabel = o6

        // Card 2: Networking & Connectivity (Compresto Glass)
        let netCard = createCard()
        detailContentView.addSubview(netCard)

        let netTitle = NSTextField(labelWithString: "NETWORKING & CONNECTIVITY")
        netTitle.translatesAutoresizingMaskIntoConstraints = false
        netTitle.font = CubeTokens.sectionHeaderFont
        netTitle.textColor = CubeTokens.secondaryText
        netCard.addSubview(netTitle)

        let copyIPBtn = NSButton(title: "Copy", target: self, action: #selector(copyIPClicked(_:)))
        copyIPBtn.translatesAutoresizingMaskIntoConstraints = false
        copyIPBtn.bezelStyle = .rounded
        copyIPBtn.controlSize = .small
        copyIPBtn.font = NSFont.systemFont(ofSize: 11)
        copyIPBtn.image = NSImage(systemSymbolName: "doc.on.doc", accessibilityDescription: nil)
        copyIPBtn.imagePosition = .imageLeading
        copyIPBtn.imageHugsTitle = true
        netCard.addSubview(copyIPBtn)

        let copySSHBtn = NSButton(title: "Copy", target: self, action: #selector(copySSHClicked(_:)))
        copySSHBtn.translatesAutoresizingMaskIntoConstraints = false
        copySSHBtn.bezelStyle = .rounded
        copySSHBtn.controlSize = .small
        copySSHBtn.font = NSFont.systemFont(ofSize: 11)
        copySSHBtn.image = NSImage(systemSymbolName: "doc.on.doc", accessibilityDescription: nil)
        copySSHBtn.imagePosition = .imageLeading
        copySSHBtn.imageHugsTitle = true
        netCard.addSubview(copySSHBtn)

        manageSharesButton = NSButton(title: "Manage...", target: self, action: #selector(manageSharesClicked))
        manageSharesButton.translatesAutoresizingMaskIntoConstraints = false
        manageSharesButton.bezelStyle = .rounded
        manageSharesButton.controlSize = .small
        manageSharesButton.font = NSFont.systemFont(ofSize: 11)
        netCard.addSubview(manageSharesButton)

        copyMountCmdButton = NSButton(title: "Copy Mount", target: self, action: #selector(copyMountCmdClicked))
        copyMountCmdButton.translatesAutoresizingMaskIntoConstraints = false
        copyMountCmdButton.bezelStyle = .rounded
        copyMountCmdButton.controlSize = .small
        copyMountCmdButton.font = NSFont.systemFont(ofSize: 11)
        copyMountCmdButton.isHidden = true
        netCard.addSubview(copyMountCmdButton)

        let (n1, nb1) = addPropertyRow(label: "Network Mode:", value: "Userland NAT Proxy", in: netCard, topAnchor: netTitle.bottomAnchor, topOffset: 12)
        netModeLabel = n1
        let (n2, nb2) = addPropertyRow(label: "Guest IP Address:", value: "192.168.64.x", in: netCard, topAnchor: nb1, topOffset: 10, trailingTarget: copyIPBtn.leadingAnchor, trailingOffset: -8)
        ipLabel = n2
        let (n3, nb3) = addPropertyRow(label: "SSH Command:", value: "ssh -p 2222 root@127.0.0.1", in: netCard, topAnchor: nb2, topOffset: 10, trailingTarget: copySSHBtn.leadingAnchor, trailingOffset: -8)
        sshCommandLabel = n3
        let (n4, nb4) = addPropertyRow(label: "VNC Display:", value: "127.0.0.1:5900 (Built-in)", in: netCard, topAnchor: nb3, topOffset: 10)
        vncLabel = n4
        let (n5, nb5) = addPropertyRow(label: "Shared Folders:", value: "None configured", in: netCard, topAnchor: nb4, topOffset: 10, trailingTarget: copyMountCmdButton.leadingAnchor, trailingOffset: -8)
        sharesListLabel = n5

        #if ENABLE_VM_TELEMETRY
        // Card 3: Real-Time Performance & Telemetry (Compresto Glass)
        let telemetryCard = createCard()
        detailContentView.addSubview(telemetryCard)

        let telemetryTitle = NSTextField(labelWithString: "REAL-TIME PERFORMANCE & TELEMETRY")
        telemetryTitle.translatesAutoresizingMaskIntoConstraints = false
        telemetryTitle.font = CubeTokens.sectionHeaderFont
        telemetryTitle.textColor = CubeTokens.secondaryText
        telemetryCard.addSubview(telemetryTitle)

        let (t1, tb1) = addPropertyRow(label: "Memory Footprint:", value: "Offline", in: telemetryCard, topAnchor: telemetryTitle.bottomAnchor, topOffset: 10)
        vmMemoryUsageLabel = t1
        let (t2, tb2) = addPropertyRow(label: "vCPU Activity:", value: "Offline", in: telemetryCard, topAnchor: tb1, topOffset: 6)
        vmCPULoadLabel = t2
        let (t3, tb3) = addPropertyRow(label: "Hypervisor Engine:", value: "Apple Silicon Hypervisor.framework (ARM64)", in: telemetryCard, topAnchor: tb2, topOffset: 6)
        vmProcessInfoLabel = t3
        #endif

        var detailConstraints: [NSLayoutConstraint] = [
            detailView.topAnchor.constraint(equalTo: detailContainerView.topAnchor),
            detailView.leadingAnchor.constraint(equalTo: detailContainerView.leadingAnchor),
            detailView.trailingAnchor.constraint(equalTo: detailContainerView.trailingAnchor),
            detailView.bottomAnchor.constraint(equalTo: detailContainerView.bottomAnchor),

            detailContentView.topAnchor.constraint(equalTo: detailView.contentView.topAnchor),
            detailContentView.leadingAnchor.constraint(equalTo: detailView.contentView.leadingAnchor),
            detailContentView.trailingAnchor.constraint(equalTo: detailView.contentView.trailingAnchor),
            detailContentView.widthAnchor.constraint(equalTo: detailView.contentView.widthAnchor),

            // VM Header
            iconBadge.topAnchor.constraint(equalTo: detailContentView.topAnchor, constant: 20),
            iconBadge.leadingAnchor.constraint(equalTo: detailContentView.leadingAnchor, constant: 28),
            iconBadge.widthAnchor.constraint(equalToConstant: 36),
            iconBadge.heightAnchor.constraint(equalToConstant: 36),

            vmNameLabel.centerYAnchor.constraint(equalTo: iconBadge.centerYAnchor),
            vmNameLabel.leadingAnchor.constraint(equalTo: iconBadge.trailingAnchor, constant: 12),

            statusBadge.centerYAnchor.constraint(equalTo: iconBadge.centerYAnchor),
            statusBadge.leadingAnchor.constraint(equalTo: vmNameLabel.trailingAnchor, constant: 10),
            statusBadge.trailingAnchor.constraint(lessThanOrEqualTo: detailContentView.trailingAnchor, constant: -28),
            statusBadge.heightAnchor.constraint(equalToConstant: 22),

            // Toolbar Stack
            toolbarStack.topAnchor.constraint(equalTo: iconBadge.bottomAnchor, constant: 16),
            toolbarStack.leadingAnchor.constraint(equalTo: detailContentView.leadingAnchor, constant: 28),
            toolbarStack.trailingAnchor.constraint(lessThanOrEqualTo: detailContentView.trailingAnchor, constant: -28),
            toolbarStack.heightAnchor.constraint(equalToConstant: 30),

            // Hardware Card
            hwCard.topAnchor.constraint(equalTo: toolbarStack.bottomAnchor, constant: 16),
            hwCard.leadingAnchor.constraint(equalTo: detailContentView.leadingAnchor, constant: 28),
            hwCard.trailingAnchor.constraint(equalTo: detailContentView.trailingAnchor, constant: -28),
            hwCard.bottomAnchor.constraint(equalTo: b6, constant: 14),

            hwTitle.topAnchor.constraint(equalTo: hwCard.topAnchor, constant: 14),
            hwTitle.leadingAnchor.constraint(equalTo: hwCard.leadingAnchor, constant: 18),
            hwTitle.trailingAnchor.constraint(lessThanOrEqualTo: showDiskBtn.leadingAnchor, constant: -12),

            showDiskBtn.trailingAnchor.constraint(equalTo: hwCard.trailingAnchor, constant: -18),
            showDiskBtn.centerYAnchor.constraint(equalTo: diskPathLabel.centerYAnchor),
            showDiskBtn.widthAnchor.constraint(equalToConstant: 105),
            showDiskBtn.heightAnchor.constraint(equalToConstant: 21),

            // Network Card
            netCard.topAnchor.constraint(equalTo: hwCard.bottomAnchor, constant: 14),
            netCard.leadingAnchor.constraint(equalTo: detailContentView.leadingAnchor, constant: 28),
            netCard.trailingAnchor.constraint(equalTo: detailContentView.trailingAnchor, constant: -28),
            netCard.bottomAnchor.constraint(equalTo: nb5, constant: 14),

            netTitle.topAnchor.constraint(equalTo: netCard.topAnchor, constant: 14),
            netTitle.leadingAnchor.constraint(equalTo: netCard.leadingAnchor, constant: 18),

            copyIPBtn.trailingAnchor.constraint(equalTo: netCard.trailingAnchor, constant: -18),
            copyIPBtn.centerYAnchor.constraint(equalTo: ipLabel.centerYAnchor),
            copyIPBtn.widthAnchor.constraint(equalToConstant: 76),
            copyIPBtn.heightAnchor.constraint(equalToConstant: 21),

            copySSHBtn.trailingAnchor.constraint(equalTo: netCard.trailingAnchor, constant: -18),
            copySSHBtn.centerYAnchor.constraint(equalTo: sshCommandLabel.centerYAnchor),
            copySSHBtn.topAnchor.constraint(greaterThanOrEqualTo: copyIPBtn.bottomAnchor, constant: 5),
            copySSHBtn.widthAnchor.constraint(equalToConstant: 76),
            copySSHBtn.heightAnchor.constraint(equalToConstant: 21),

            manageSharesButton.trailingAnchor.constraint(equalTo: netCard.trailingAnchor, constant: -18),
            manageSharesButton.centerYAnchor.constraint(equalTo: sharesListLabel.centerYAnchor),
            manageSharesButton.widthAnchor.constraint(equalToConstant: 92),
            manageSharesButton.heightAnchor.constraint(equalToConstant: 21),

            copyMountCmdButton.trailingAnchor.constraint(equalTo: manageSharesButton.leadingAnchor, constant: -8),
            copyMountCmdButton.centerYAnchor.constraint(equalTo: sharesListLabel.centerYAnchor),
            copyMountCmdButton.widthAnchor.constraint(equalToConstant: 88),
            copyMountCmdButton.heightAnchor.constraint(equalToConstant: 21)
        ]

        let badgeWidth = statusBadge.widthAnchor.constraint(equalToConstant: 90)
        badgeWidth.isActive = true
        statusBadgeWidthConstraint = badgeWidth

        #if ENABLE_VM_TELEMETRY
        detailConstraints.append(contentsOf: [
            telemetryCard.topAnchor.constraint(equalTo: netCard.bottomAnchor, constant: 14),
            telemetryCard.leadingAnchor.constraint(equalTo: detailContentView.leadingAnchor, constant: 28),
            telemetryCard.trailingAnchor.constraint(equalTo: detailContentView.trailingAnchor, constant: -28),
            telemetryCard.bottomAnchor.constraint(equalTo: tb3, constant: 14),

            telemetryTitle.topAnchor.constraint(equalTo: telemetryCard.topAnchor, constant: 14),
            telemetryTitle.leadingAnchor.constraint(equalTo: telemetryCard.leadingAnchor, constant: 18),

            detailContentView.bottomAnchor.constraint(equalTo: telemetryCard.bottomAnchor, constant: 28)
        ])
        #else
        detailConstraints.append(
            detailContentView.bottomAnchor.constraint(equalTo: netCard.bottomAnchor, constant: 28)
        )
        #endif

        NSLayoutConstraint.activate(detailConstraints)
    }

    private func createCard() -> DashboardCardView {
        return DashboardCardView()
    }

    private func createQuickStartCard(icon: String, iconColor: NSColor, title: String, text: String) -> NSView {
        let card = DashboardCardView()

        let iconView = NSImageView()
        iconView.translatesAutoresizingMaskIntoConstraints = false
        let cfg = NSImage.SymbolConfiguration(pointSize: 16, weight: .semibold)
        if let img = NSImage(systemSymbolName: icon, accessibilityDescription: nil)?.withSymbolConfiguration(cfg) {
            iconView.image = img
            iconView.contentTintColor = iconColor
        }
        card.addSubview(iconView)

        let titleLabel = NSTextField(labelWithString: title)
        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.font = NSFont.boldSystemFont(ofSize: 12.5)
        titleLabel.textColor = CubeTokens.primaryText
        card.addSubview(titleLabel)

        let descLabel = NSTextField(wrappingLabelWithString: text)
        descLabel.translatesAutoresizingMaskIntoConstraints = false
        descLabel.font = NSFont.systemFont(ofSize: 11)
        descLabel.textColor = CubeTokens.secondaryText
        card.addSubview(descLabel)

        NSLayoutConstraint.activate([
            iconView.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            iconView.topAnchor.constraint(equalTo: card.topAnchor, constant: 14),
            iconView.widthAnchor.constraint(equalToConstant: 20),
            iconView.heightAnchor.constraint(equalToConstant: 20),

            titleLabel.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 8),
            titleLabel.centerYAnchor.constraint(equalTo: iconView.centerYAnchor),
            titleLabel.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -10),

            descLabel.topAnchor.constraint(equalTo: iconView.bottomAnchor, constant: 10),
            descLabel.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            descLabel.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            descLabel.bottomAnchor.constraint(lessThanOrEqualTo: card.bottomAnchor, constant: -12)
        ])

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

    private func addPropertyRow(
        label: String,
        value: String,
        in card: NSView,
        topAnchor: NSLayoutYAxisAnchor,
        topOffset: CGFloat = 8,
        trailingTarget: NSLayoutXAxisAnchor? = nil,
        trailingOffset: CGFloat = -18
    ) -> (NSTextField, NSLayoutYAxisAnchor) {
        let lbl = NSTextField(labelWithString: label)
        lbl.translatesAutoresizingMaskIntoConstraints = false
        lbl.font = CubeTokens.bodyLabelFont
        lbl.textColor = CubeTokens.secondaryText
        card.addSubview(lbl)

        let val = NSTextField(labelWithString: value)
        val.translatesAutoresizingMaskIntoConstraints = false
        val.font = CubeTokens.bodyValueFont
        val.textColor = CubeTokens.primaryText
        val.isSelectable = true
        val.lineBreakMode = .byTruncatingMiddle
        card.addSubview(val)

        let targetTrailing = trailingTarget ?? card.trailingAnchor

        NSLayoutConstraint.activate([
            lbl.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 18),
            lbl.topAnchor.constraint(equalTo: topAnchor, constant: topOffset),
            lbl.widthAnchor.constraint(equalToConstant: 125),

            val.leadingAnchor.constraint(equalTo: lbl.trailingAnchor, constant: 8),
            val.centerYAnchor.constraint(equalTo: lbl.centerYAnchor),
            val.trailingAnchor.constraint(lessThanOrEqualTo: targetTrailing, constant: trailingOffset)
        ])

        return (val, lbl.bottomAnchor)
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

        let btnSymbolConfig = NSImage.SymbolConfiguration(pointSize: 11.5, weight: .medium)

        if isRunning {
            let pidStr = vm.pid.map { " (PID \($0))" } ?? ""
            badgeText = "● RUNNING\(pidStr)"
            badgeColor = CubeTokens.runningGreen
            bgColor = CubeTokens.runningGreen.withAlphaComponent(0.18)
            statusBadge.layer?.borderColor = CubeTokens.runningGreen.withAlphaComponent(0.55).cgColor
            startStopButton.title = "Stop VM"
            startStopButton.image = NSImage(systemSymbolName: "power", accessibilityDescription: nil)?.withSymbolConfiguration(btnSymbolConfig)
            startStopButton.imagePosition = .imageLeading
            startStopButton.imageHugsTitle = true
            sshButton.isEnabled = true
            vncButton.isEnabled = true
            commandsButton.isEnabled = true
        } else {
            badgeText = "○ STOPPED"
            badgeColor = CubeTokens.secondaryText
            bgColor = CubeTokens.capsuleBackground
            statusBadge.layer?.borderColor = CubeTokens.capsuleBorder.cgColor
            startStopButton.title = "Start VM"
            startStopButton.image = NSImage(systemSymbolName: "play.fill", accessibilityDescription: nil)?.withSymbolConfiguration(btnSymbolConfig)
            startStopButton.imagePosition = .imageLeading
            startStopButton.imageHugsTitle = true
            sshButton.isEnabled = true
            vncButton.isEnabled = true
            commandsButton.isEnabled = true
        }

        statusBadge.stringValue = badgeText
        statusBadge.textColor = badgeColor
        statusBadge.layer?.backgroundColor = bgColor.cgColor

        let font = statusBadge.font ?? CubeTokens.badgeFont
        let textWidth = (badgeText as NSString).size(withAttributes: [.font: font]).width
        let badgeWidth = max(70, ceil(textWidth) + 16)
        statusBadgeWidthConstraint?.constant = badgeWidth

        let cfg = vm.loadConfig()
        let os = vm.os ?? cfg.os ?? "FreeBSD"
        osVersionLabel.stringValue = "\(os) (ARM64)"
        archLabel.stringValue = "ARM64 (Apple Silicon Hypervisor)"
        cpusLabel.stringValue = "\(cfg.cpus) vCPUs"
        let memMode = cfg.memPrealloc ? "Pre-allocated" : "On-Demand"
        ramLabel.stringValue = "\(cfg.ramMB) MB (\(memMode))"
        bootModeLabel.stringValue = cfg.bootMode.uppercased()

        let rawDisk = cfg.diskPath ?? "rootfs.img"
        let filename = (rawDisk as NSString).lastPathComponent
        let fullDiskPath = rawDisk.hasPrefix("/") ? rawDisk : "\(VMManager.shared.vmsDirectory)/\(vm.name)/\(rawDisk)"
        diskPathLabel.stringValue = formatDiskDescription(filename: filename, fullPath: fullDiskPath)
        diskPathLabel.toolTip = fullDiskPath

        netModeLabel.stringValue = cfg.netMode
        ipLabel.stringValue = vm.ip ?? (isRunning ? "Acquiring DHCP..." : "Offline")

        let port = vm.ssh_port ?? cfg.sshPort ?? 2222
        let host = (vm.ssh_port != nil || cfg.sshPort != nil) ? "127.0.0.1" : (vm.ip ?? "127.0.0.1")
        let fullCmd = "ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p \(port) root@\(host)"
        sshCommandLabel.stringValue = fullCmd
        sshCommandLabel.toolTip = fullCmd

        if let vnc = vm.vnc ?? cfg.vnc {
            vncLabel.stringValue = vnc
            vncButton.isEnabled = true
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

    private func formatDiskDescription(filename: String, fullPath: String) -> String {
        let fm = FileManager.default
        guard fm.fileExists(atPath: fullPath) else {
            return filename
        }

        var logicalBytes: Int64 = 0
        if let attr = try? fm.attributesOfItem(atPath: fullPath),
           let num = attr[.size] as? NSNumber {
            logicalBytes = num.int64Value
        }

        var physicalBytes: Int64 = 0
        let url = URL(fileURLWithPath: fullPath)
        if let values = try? url.resourceValues(forKeys: [.totalFileAllocatedSizeKey]),
           let alloc = values.totalFileAllocatedSize {
            physicalBytes = Int64(alloc)
        } else if let attr = try? fm.attributesOfItem(atPath: fullPath),
                  let num = attr[.size] as? NSNumber {
            physicalBytes = num.int64Value
        }

        if logicalBytes > 0 {
            let logicalStr = formatStorageBytes(logicalBytes)
            if physicalBytes > 0 && physicalBytes < logicalBytes {
                let physicalStr = formatStorageBytes(physicalBytes)
                return "\(filename) — \(logicalStr) (\(physicalStr) on disk)"
            } else {
                return "\(filename) — \(logicalStr)"
            }
        }
        return filename
    }

    private func formatStorageBytes(_ bytes: Int64) -> String {
        let b = Double(bytes)
        let gib = b / (1024 * 1024 * 1024)
        if gib >= 1.0 {
            if gib.truncatingRemainder(dividingBy: 1.0) == 0 {
                return String(format: "%.0f GB", gib)
            } else if gib < 10.0 {
                return String(format: "%.1f GB", gib)
            } else {
                return String(format: "%.0f GB", round(gib))
            }
        }
        let mib = b / (1024 * 1024)
        if mib >= 1.0 {
            return String(format: "%.0f MB", mib)
        }
        return "\(bytes) B"
    }

    private func updateServiceBanner() {
        let isRunning = ServiceManager.shared.isRunning
        if isRunning {
            let mode = (ServiceManager.shared.activeMode ?? ServiceManager.shared.currentMode).description
            serviceStatusDot.stringValue = "●"
            serviceStatusDot.textColor = CubeTokens.runningGreen
            let text = "Cube Network Service: Active (\(mode))"
            serviceStatusLabel.stringValue = text
            serviceStatusLabel.toolTip = text
            serviceStatusLabel.textColor = CubeTokens.primaryText
            serviceToggleButton.title = "Stop Service"
        } else {
            serviceStatusDot.stringValue = "○"
            serviceStatusDot.textColor = CubeTokens.stoppedGray
            let text = "Cube Network Service: Stopped"
            serviceStatusLabel.stringValue = text
            serviceStatusLabel.toolTip = text
            serviceStatusLabel.textColor = CubeTokens.secondaryText
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
            let v = NSView()
            v.identifier = cellId

            let dot = NSTextField(labelWithString: "●")
            dot.translatesAutoresizingMaskIntoConstraints = false
            dot.tag = 101
            dot.font = NSFont.systemFont(ofSize: 9)
            v.addSubview(dot)

            let title = NSTextField(labelWithString: "")
            title.translatesAutoresizingMaskIntoConstraints = false
            title.tag = 102
            title.font = CubeTokens.sidebarTitleFont
            title.lineBreakMode = .byTruncatingTail
            v.addSubview(title)

            let subtitle = NSTextField(labelWithString: "")
            subtitle.translatesAutoresizingMaskIntoConstraints = false
            subtitle.tag = 103
            subtitle.font = CubeTokens.sidebarSubtitleFont
            subtitle.textColor = CubeTokens.secondaryText
            subtitle.lineBreakMode = .byTruncatingTail
            v.addSubview(subtitle)

            NSLayoutConstraint.activate([
                dot.leadingAnchor.constraint(equalTo: v.leadingAnchor, constant: 12),
                dot.centerYAnchor.constraint(equalTo: v.centerYAnchor),
                dot.widthAnchor.constraint(equalToConstant: 12),

                title.leadingAnchor.constraint(equalTo: dot.trailingAnchor, constant: 6),
                title.topAnchor.constraint(equalTo: v.topAnchor, constant: 7),
                title.trailingAnchor.constraint(equalTo: v.trailingAnchor, constant: -12),

                subtitle.leadingAnchor.constraint(equalTo: title.leadingAnchor),
                subtitle.topAnchor.constraint(equalTo: title.bottomAnchor, constant: 2),
                subtitle.trailingAnchor.constraint(equalTo: v.trailingAnchor, constant: -12)
            ])

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

    // MARK: - NSSplitViewDelegate

    func splitView(_ splitView: NSSplitView, canCollapseSubview subview: NSView) -> Bool {
        return false
    }

    func splitView(_ splitView: NSSplitView, constrainMinCoordinate proposedMinimumPosition: CGFloat, ofSubviewAt dividerIndex: Int) -> CGFloat {
        return 200
    }

    func splitView(_ splitView: NSSplitView, constrainMaxCoordinate proposedMaximumPosition: CGFloat, ofSubviewAt dividerIndex: Int) -> CGFloat {
        return 340
    }

    func splitView(_ splitView: NSSplitView, shouldAdjustSizeOfSubview view: NSView) -> Bool {
        return view !== sidebarView
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

    @objc private func commandsClicked() {
        guard let name = selectedVMName,
              let vm = VMManager.shared.vms.first(where: { $0.name == name }),
              let win = self.window else { return }
        VMCommandsSheetController.show(for: vm, on: win)
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
            CubeTokens.animateCopyFeedback(button: btn)
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
            CubeTokens.animateCopyFeedback(button: btn)
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

        CubeTokens.animateCopyFeedback(
            button: copyMountCmdButton,
            temporaryTitle: "Copied ✓",
            temporaryIcon: "checkmark",
            originalTitle: "Copy Mount Command",
            originalIcon: "doc.on.doc",
            duration: 1.5
        )
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

        rootVisualEffect.material = reduceTransparency ? .windowBackground : .underWindowBackground
        rootVisualEffect.blendingMode = reduceTransparency ? .withinWindow : .behindWindow
        rootVisualEffect.state = reduceTransparency ? .inactive : .active
        rootVisualEffect.layer?.borderColor = CubeTokens.windowBorder.cgColor

        sidebarView.material = reduceTransparency ? .windowBackground : .sidebar
        sidebarView.blendingMode = reduceTransparency ? .withinWindow : .behindWindow
        sidebarView.state = reduceTransparency ? .inactive : .active

        statusBarView.layer?.backgroundColor = CubeTokens.statusBarBackground.cgColor

        refreshCardLayers(in: window.contentView)
        window.contentView?.needsDisplay = true
    }

    private func refreshCardLayers(in view: NSView?) {
        guard let view = view else { return }
        if let card = view as? DashboardCardView {
            card.layer?.backgroundColor = CubeTokens.cardBackground.cgColor
            card.layer?.borderColor = CubeTokens.cardBorder.cgColor
        }
        for sub in view.subviews {
            refreshCardLayers(in: sub)
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
        tagLabel.translatesAutoresizingMaskIntoConstraints = false
        tagLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        accessory.addSubview(tagLabel)

        let tagField = NSTextField()
        tagField.translatesAutoresizingMaskIntoConstraints = false
        tagField.placeholderString = defaultTag ?? "e.g. host-share (default: folder name)"
        if let defaultTag = defaultTag {
            tagField.stringValue = defaultTag
        }
        accessory.addSubview(tagField)

        let roBox = NSButton(checkboxWithTitle: "Read-Only access inside guest VM", target: nil, action: nil)
        roBox.translatesAutoresizingMaskIntoConstraints = false
        roBox.font = NSFont.systemFont(ofSize: 11)
        accessory.addSubview(roBox)

        NSLayoutConstraint.activate([
            tagLabel.leadingAnchor.constraint(equalTo: accessory.leadingAnchor),
            tagLabel.centerYAnchor.constraint(equalTo: tagField.centerYAnchor),
            tagLabel.widthAnchor.constraint(equalToConstant: 70),

            tagField.leadingAnchor.constraint(equalTo: tagLabel.trailingAnchor, constant: 5),
            tagField.trailingAnchor.constraint(equalTo: accessory.trailingAnchor, constant: -10),
            tagField.topAnchor.constraint(equalTo: accessory.topAnchor, constant: 6),
            tagField.heightAnchor.constraint(equalToConstant: 22),

            roBox.leadingAnchor.constraint(equalTo: tagField.leadingAnchor),
            roBox.trailingAnchor.constraint(equalTo: accessory.trailingAnchor, constant: -10),
            roBox.topAnchor.constraint(equalTo: tagField.bottomAnchor, constant: 6)
        ])

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
