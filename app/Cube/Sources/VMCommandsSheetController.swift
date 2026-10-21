import Foundation
import AppKit

private class SheetCardView: NSView {
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

class VMCommandsSheetController: NSWindowController, NSWindowDelegate {
    static var current: VMCommandsSheetController?

    private let vm: VMInfo
    private weak var parentSheetWindow: NSWindow?

    private var rootVisualEffect: NSVisualEffectView!
    private var stackView: NSStackView!
    private var bottomBarView: NSView!

    deinit {
        NSWorkspace.shared.notificationCenter.removeObserver(self)
    }

    init(vm: VMInfo, parent: NSWindow? = nil) {
        self.vm = vm
        self.parentSheetWindow = parent

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 700, height: 600),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "\(vm.name) — CLI Commands & Launch Info"
        window.minSize = NSSize(width: 560, height: 420)
        window.center()
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self

        setupUI()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func show(for vm: VMInfo, on parentWindow: NSWindow? = nil) {
        if let existing = current {
            if existing.vm.name == vm.name && existing.vm.config == vm.config {
                AppDelegate.ensureRegularActivation()
                existing.window?.makeKeyAndOrderFront(nil)
                NSApp.activate(ignoringOtherApps: true)
                return
            }
            existing.close()
        }
        AppDelegate.ensureRegularActivation()
        let controller = VMCommandsSheetController(vm: vm, parent: nil)
        current = controller
        controller.showWindow(nil)
        controller.window?.center()
        controller.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    override func cancelOperation(_ sender: Any?) {
        doneClicked()
    }

    func windowWillClose(_ notification: Notification) {
        if VMCommandsSheetController.current === self {
            VMCommandsSheetController.current = nil
        }
        let hasOtherWindows = PreferencesWindowController.shared.window?.isVisible == true ||
                              DashboardWindowController.shared.window?.isVisible == true ||
                              (NewVMWindowController.shared?.window?.isVisible == true) ||
                              (VMLogWindowController.shared.window?.isVisible == true)
        if !hasOtherWindows {
            NSApp.setActivationPolicy(.accessory)
        }
    }

    // MARK: - UI Construction (Auto Layout & Frosted Glass)

    private func setupUI() {
        guard let window = window else { return }

        // Root Liquid Translucent Glass (underWindowBackground blur behind window)
        rootVisualEffect = NSVisualEffectView(frame: window.contentView?.bounds ?? NSRect(x: 0, y: 0, width: 620, height: 580))
        rootVisualEffect.material = .underWindowBackground
        rootVisualEffect.blendingMode = .behindWindow
        rootVisualEffect.state = .active
        rootVisualEffect.autoresizingMask = [.width, .height]
        rootVisualEffect.wantsLayer = true
        window.contentView = rootVisualEffect

        // 1. Header Bar: Icon, Name, Subtitle
        let headerView = NSView()
        headerView.translatesAutoresizingMaskIntoConstraints = false
        rootVisualEffect.addSubview(headerView)

        let iconView = NSImageView()
        iconView.translatesAutoresizingMaskIntoConstraints = false
        let cfg = NSImage.SymbolConfiguration(pointSize: 24, weight: .regular)
        if let img = NSImage(systemSymbolName: "terminal.fill", accessibilityDescription: nil)?.withSymbolConfiguration(cfg) {
            iconView.image = img
            iconView.contentTintColor = CubeTokens.accent
        }
        headerView.addSubview(iconView)

        let titleLabel = NSTextField(labelWithString: vm.name)
        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.font = NSFont.systemFont(ofSize: 18, weight: .bold)
        titleLabel.textColor = CubeTokens.primaryText
        headerView.addSubview(titleLabel)

        let subtitleLabel = NSTextField(labelWithString: "CLI Execution & Automation Commands")
        subtitleLabel.translatesAutoresizingMaskIntoConstraints = false
        subtitleLabel.font = NSFont.systemFont(ofSize: 11.5, weight: .regular)
        subtitleLabel.textColor = CubeTokens.secondaryText
        headerView.addSubview(subtitleLabel)

        let sepHeader = NSBox()
        sepHeader.translatesAutoresizingMaskIntoConstraints = false
        sepHeader.boxType = .custom
        sepHeader.borderWidth = 0
        sepHeader.fillColor = CubeTokens.separatorColor
        headerView.addSubview(sepHeader)

        // 2. Bottom Bar with Done Button
        bottomBarView = NSView()
        bottomBarView.translatesAutoresizingMaskIntoConstraints = false
        bottomBarView.wantsLayer = true
        bottomBarView.layer?.backgroundColor = CubeTokens.statusBarBackground.cgColor
        rootVisualEffect.addSubview(bottomBarView)

        let sepBottom = NSBox()
        sepBottom.translatesAutoresizingMaskIntoConstraints = false
        sepBottom.boxType = .custom
        sepBottom.borderWidth = 0
        sepBottom.fillColor = CubeTokens.separatorColor
        bottomBarView.addSubview(sepBottom)

        let doneButton = NSButton(title: "Done", target: self, action: #selector(doneClicked))
        doneButton.translatesAutoresizingMaskIntoConstraints = false
        doneButton.bezelStyle = .rounded
        doneButton.keyEquivalent = "\r"
        doneButton.font = NSFont.systemFont(ofSize: 12, weight: .semibold)
        bottomBarView.addSubview(doneButton)

        // 3. Middle Scroll View for Cards
        let scrollView = NSScrollView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.drawsBackground = false
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        rootVisualEffect.addSubview(scrollView)

        let scrollContentView = NSView()
        scrollContentView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.documentView = scrollContentView

        stackView = NSStackView()
        stackView.translatesAutoresizingMaskIntoConstraints = false
        stackView.orientation = .vertical
        stackView.alignment = .leading
        stackView.distribution = .fill
        stackView.spacing = 14
        scrollContentView.addSubview(stackView)

        NSLayoutConstraint.activate([
            // Header Layout
            headerView.topAnchor.constraint(equalTo: rootVisualEffect.topAnchor),
            headerView.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            headerView.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            headerView.heightAnchor.constraint(equalToConstant: 74),

            iconView.leadingAnchor.constraint(equalTo: headerView.leadingAnchor, constant: 22),
            iconView.topAnchor.constraint(equalTo: headerView.topAnchor, constant: 22),
            iconView.widthAnchor.constraint(equalToConstant: 30),
            iconView.heightAnchor.constraint(equalToConstant: 30),

            titleLabel.leadingAnchor.constraint(equalTo: iconView.trailingAnchor, constant: 12),
            titleLabel.topAnchor.constraint(equalTo: headerView.topAnchor, constant: 18),
            titleLabel.trailingAnchor.constraint(equalTo: headerView.trailingAnchor, constant: -20),

            subtitleLabel.leadingAnchor.constraint(equalTo: titleLabel.leadingAnchor),
            subtitleLabel.topAnchor.constraint(equalTo: titleLabel.bottomAnchor, constant: 2),
            subtitleLabel.trailingAnchor.constraint(equalTo: headerView.trailingAnchor, constant: -20),

            sepHeader.leadingAnchor.constraint(equalTo: headerView.leadingAnchor),
            sepHeader.trailingAnchor.constraint(equalTo: headerView.trailingAnchor),
            sepHeader.bottomAnchor.constraint(equalTo: headerView.bottomAnchor),
            sepHeader.heightAnchor.constraint(equalToConstant: 1),

            // Bottom Bar Layout
            bottomBarView.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            bottomBarView.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            bottomBarView.bottomAnchor.constraint(equalTo: rootVisualEffect.bottomAnchor),
            bottomBarView.heightAnchor.constraint(equalToConstant: 50),

            sepBottom.leadingAnchor.constraint(equalTo: bottomBarView.leadingAnchor),
            sepBottom.trailingAnchor.constraint(equalTo: bottomBarView.trailingAnchor),
            sepBottom.topAnchor.constraint(equalTo: bottomBarView.topAnchor),
            sepBottom.heightAnchor.constraint(equalToConstant: 1),

            doneButton.trailingAnchor.constraint(equalTo: bottomBarView.trailingAnchor, constant: -20),
            doneButton.centerYAnchor.constraint(equalTo: bottomBarView.centerYAnchor),
            doneButton.widthAnchor.constraint(equalToConstant: 80),

            // Middle Scroll View Layout
            scrollView.topAnchor.constraint(equalTo: headerView.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: bottomBarView.topAnchor),

            scrollContentView.topAnchor.constraint(equalTo: scrollView.contentView.topAnchor),
            scrollContentView.leadingAnchor.constraint(equalTo: scrollView.contentView.leadingAnchor),
            scrollContentView.trailingAnchor.constraint(equalTo: scrollView.contentView.trailingAnchor),
            scrollContentView.bottomAnchor.constraint(equalTo: scrollView.contentView.bottomAnchor),
            scrollContentView.widthAnchor.constraint(equalTo: scrollView.contentView.widthAnchor),

            stackView.topAnchor.constraint(equalTo: scrollContentView.topAnchor, constant: 14),
            stackView.leadingAnchor.constraint(equalTo: scrollContentView.leadingAnchor, constant: 20),
            stackView.trailingAnchor.constraint(equalTo: scrollContentView.trailingAnchor, constant: -20),
            stackView.bottomAnchor.constraint(equalTo: scrollContentView.bottomAnchor, constant: -16)
        ])

        // Build Cards
        buildCubectlCard()
        buildCubeEngineCard()
        buildStorageCard()

        // Accessibility Listeners
        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(accessibilityDisplayOptionsDidChange),
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification,
            object: nil
        )
        updateAccessibilityPreferences()
    }

    // MARK: - Card Construction Helpers

    private func addCardHeader(title: String, to card: NSView) -> NSLayoutYAxisAnchor {
        let label = NSTextField(labelWithString: title.uppercased())
        label.translatesAutoresizingMaskIntoConstraints = false
        label.font = CubeTokens.sectionHeaderFont
        label.textColor = CubeTokens.secondaryText
        card.addSubview(label)

        let sep = NSBox()
        sep.translatesAutoresizingMaskIntoConstraints = false
        sep.boxType = .custom
        sep.borderWidth = 0
        sep.fillColor = CubeTokens.separatorColor
        card.addSubview(sep)

        NSLayoutConstraint.activate([
            label.topAnchor.constraint(equalTo: card.topAnchor, constant: 12),
            label.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            label.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),

            sep.topAnchor.constraint(equalTo: label.bottomAnchor, constant: 8),
            sep.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            sep.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            sep.heightAnchor.constraint(equalToConstant: 1)
        ])

        return sep.bottomAnchor
    }

    private func addCommandRow(label: String, command: String, in card: NSView, topAnchor: NSLayoutYAxisAnchor) -> NSLayoutYAxisAnchor {
        let rowView = NSView()
        rowView.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(rowView)

        let lbl = NSTextField(labelWithString: label)
        lbl.translatesAutoresizingMaskIntoConstraints = false
        lbl.font = CubeTokens.bodyLabelFont
        lbl.textColor = CubeTokens.secondaryText
        lbl.alignment = .right
        rowView.addSubview(lbl)

        let cmdBox = NSTextField(labelWithString: command)
        cmdBox.translatesAutoresizingMaskIntoConstraints = false
        cmdBox.font = CubeTokens.bodyValueFont
        cmdBox.textColor = CubeTokens.primaryText
        cmdBox.lineBreakMode = .byTruncatingTail
        rowView.addSubview(cmdBox)

        let copyBtn = NSButton(title: "Copy", target: self, action: #selector(copyCommandClicked(_:)))
        copyBtn.translatesAutoresizingMaskIntoConstraints = false
        copyBtn.bezelStyle = .rounded
        copyBtn.controlSize = .small
        copyBtn.font = NSFont.systemFont(ofSize: 11)
        copyBtn.toolTip = "Copy: \(command)"
        rowView.addSubview(copyBtn)

        NSLayoutConstraint.activate([
            rowView.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            rowView.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            rowView.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            rowView.heightAnchor.constraint(equalToConstant: 24),

            lbl.leadingAnchor.constraint(equalTo: rowView.leadingAnchor),
            lbl.centerYAnchor.constraint(equalTo: rowView.centerYAnchor),
            lbl.widthAnchor.constraint(equalToConstant: 120),

            cmdBox.leadingAnchor.constraint(equalTo: lbl.trailingAnchor, constant: 12),
            cmdBox.centerYAnchor.constraint(equalTo: rowView.centerYAnchor),
            cmdBox.trailingAnchor.constraint(lessThanOrEqualTo: copyBtn.leadingAnchor, constant: -8),

            copyBtn.trailingAnchor.constraint(equalTo: rowView.trailingAnchor),
            copyBtn.centerYAnchor.constraint(equalTo: rowView.centerYAnchor),
            copyBtn.widthAnchor.constraint(equalToConstant: 62)
        ])

        return rowView.bottomAnchor
    }

    private func addPathRow(label: String, path: String, in card: NSView, topAnchor: NSLayoutYAxisAnchor) -> NSLayoutYAxisAnchor {
        let rowView = NSView()
        rowView.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(rowView)

        let lbl = NSTextField(labelWithString: label)
        lbl.translatesAutoresizingMaskIntoConstraints = false
        lbl.font = CubeTokens.bodyLabelFont
        lbl.textColor = CubeTokens.secondaryText
        lbl.alignment = .right
        rowView.addSubview(lbl)

        let pathBox = NSTextField(labelWithString: path)
        pathBox.translatesAutoresizingMaskIntoConstraints = false
        pathBox.font = NSFont.systemFont(ofSize: 11, weight: .regular)
        pathBox.textColor = CubeTokens.primaryText
        pathBox.lineBreakMode = .byTruncatingMiddle
        pathBox.toolTip = path
        rowView.addSubview(pathBox)

        let revealBtn = NSButton(title: "Reveal", target: self, action: #selector(revealPathClicked(_:)))
        revealBtn.translatesAutoresizingMaskIntoConstraints = false
        revealBtn.bezelStyle = .rounded
        revealBtn.controlSize = .small
        revealBtn.font = NSFont.systemFont(ofSize: 11)
        revealBtn.toolTip = "Reveal in Finder: \(path)"
        rowView.addSubview(revealBtn)

        NSLayoutConstraint.activate([
            rowView.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            rowView.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            rowView.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            rowView.heightAnchor.constraint(equalToConstant: 24),

            lbl.leadingAnchor.constraint(equalTo: rowView.leadingAnchor),
            lbl.centerYAnchor.constraint(equalTo: rowView.centerYAnchor),
            lbl.widthAnchor.constraint(equalToConstant: 120),

            pathBox.leadingAnchor.constraint(equalTo: lbl.trailingAnchor, constant: 12),
            pathBox.centerYAnchor.constraint(equalTo: rowView.centerYAnchor),
            pathBox.trailingAnchor.constraint(lessThanOrEqualTo: revealBtn.leadingAnchor, constant: -8),

            revealBtn.trailingAnchor.constraint(equalTo: rowView.trailingAnchor),
            revealBtn.centerYAnchor.constraint(equalTo: rowView.centerYAnchor),
            revealBtn.widthAnchor.constraint(equalToConstant: 62)
        ])

        return rowView.bottomAnchor
    }

    // MARK: - Card 1: cubectl Daemon Commands

    private func buildCubectlCard() {
        let card = SheetCardView()
        stackView.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: stackView.widthAnchor).isActive = true

        var lastAnchor = addCardHeader(title: "Cubectl Manager Commands", to: card)
        lastAnchor = addCommandRow(label: "Start VM:", command: "cubectl start \(vm.name)", in: card, topAnchor: lastAnchor)
        lastAnchor = addCommandRow(label: "Connect via SSH:", command: "cubectl ssh \(vm.name)", in: card, topAnchor: lastAnchor)
        lastAnchor = addCommandRow(label: "Query Status:", command: "cubectl status \(vm.name)", in: card, topAnchor: lastAnchor)
        lastAnchor = addCommandRow(label: "Stop VM:", command: "cubectl stop \(vm.name)", in: card, topAnchor: lastAnchor)

        card.bottomAnchor.constraint(equalTo: lastAnchor, constant: 14).isActive = true
    }

    // MARK: - Card 2: Direct Hypervisor Engine Command (cube)

    private func buildCubeEngineCard() {
        let card = SheetCardView()
        stackView.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: stackView.widthAnchor).isActive = true

        let headerAnchor = addCardHeader(title: "Direct Hypervisor Engine Command (Cube)", to: card)

        let c2Sub = NSTextField(labelWithString: "Run directly via Apple Silicon Hypervisor.framework without manager daemon:")
        c2Sub.translatesAutoresizingMaskIntoConstraints = false
        c2Sub.font = NSFont.systemFont(ofSize: 11, weight: .regular)
        c2Sub.textColor = CubeTokens.tertiaryText
        card.addSubview(c2Sub)

        let cubeCmd = vm.generateCubeCommand()
        let cmdScrollView = NSScrollView()
        cmdScrollView.translatesAutoresizingMaskIntoConstraints = false
        cmdScrollView.wantsLayer = true
        cmdScrollView.layer?.cornerRadius = 6
        cmdScrollView.layer?.borderWidth = 1.0
        cmdScrollView.layer?.borderColor = CubeTokens.cardBorder.cgColor
        cmdScrollView.backgroundColor = NSColor(white: 0.08, alpha: 1.0)
        cmdScrollView.drawsBackground = true
        cmdScrollView.hasVerticalScroller = true
        cmdScrollView.autohidesScrollers = true
        card.addSubview(cmdScrollView)

        let cmdTextView = NSTextView()
        cmdTextView.string = cubeCmd
        cmdTextView.font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .regular)
        cmdTextView.textColor = NSColor(red: 0.90, green: 0.94, blue: 0.98, alpha: 1.0)
        cmdTextView.backgroundColor = NSColor(white: 0.08, alpha: 1.0)
        cmdTextView.drawsBackground = false
        cmdTextView.isEditable = false
        cmdTextView.isSelectable = true
        cmdTextView.isVerticallyResizable = true
        cmdTextView.isHorizontallyResizable = false
        cmdTextView.textContainer?.widthTracksTextView = true
        cmdScrollView.documentView = cmdTextView

        let copyEngineBtn = NSButton(title: "Copy Direct Command", target: self, action: #selector(copyEngineClicked(_:)))
        copyEngineBtn.translatesAutoresizingMaskIntoConstraints = false
        copyEngineBtn.bezelStyle = .rounded
        copyEngineBtn.controlSize = .small
        copyEngineBtn.font = NSFont.systemFont(ofSize: 11)
        card.addSubview(copyEngineBtn)

        NSLayoutConstraint.activate([
            c2Sub.topAnchor.constraint(equalTo: headerAnchor, constant: 8),
            c2Sub.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            c2Sub.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),

            cmdScrollView.topAnchor.constraint(equalTo: c2Sub.bottomAnchor, constant: 6),
            cmdScrollView.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            cmdScrollView.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            cmdScrollView.heightAnchor.constraint(equalToConstant: 80),

            copyEngineBtn.topAnchor.constraint(equalTo: cmdScrollView.bottomAnchor, constant: 8),
            copyEngineBtn.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            copyEngineBtn.heightAnchor.constraint(equalToConstant: 24),
            copyEngineBtn.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -14)
        ])
    }

    // MARK: - Card 3: Storage Locations

    private func buildStorageCard() {
        let card = SheetCardView()
        stackView.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: stackView.widthAnchor).isActive = true

        var lastAnchor = addCardHeader(title: "Storage & File Locations", to: card)

        let configPath = vm.config ?? "\(VMManager.shared.vmsDirectory)/\(vm.name)/\(vm.name).json"
        let statePath = "\(NSHomeDirectory())/.local/state/cube/\(vm.name)"

        lastAnchor = addPathRow(label: "Config File:", path: configPath, in: card, topAnchor: lastAnchor)
        lastAnchor = addPathRow(label: "Runtime State:", path: statePath, in: card, topAnchor: lastAnchor)

        card.bottomAnchor.constraint(equalTo: lastAnchor, constant: 14).isActive = true
    }

    // MARK: - Actions

    @objc private func copyCommandClicked(_ sender: NSButton) {
        if let tooltip = sender.toolTip, tooltip.hasPrefix("Copy: ") {
            let cmd = String(tooltip.dropFirst(6))
            let pb = NSPasteboard.general
            pb.clearContents()
            pb.setString(cmd, forType: .string)
            CubeTokens.animateCopyFeedback(button: sender)
        }
    }

    @objc private func copyEngineClicked(_ sender: NSButton) {
        let cubeCmd = vm.generateCubeCommand()
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(cubeCmd, forType: .string)
        CubeTokens.animateCopyFeedback(
            button: sender,
            temporaryTitle: "Copied! ✓",
            temporaryIcon: "checkmark",
            originalTitle: "Copy Direct Command",
            originalIcon: "doc.on.doc",
            duration: 1.2
        )
    }

    @objc private func revealPathClicked(_ sender: NSButton) {
        if let tooltip = sender.toolTip, tooltip.hasPrefix("Reveal in Finder: ") {
            let path = String(tooltip.dropFirst(18))
            if FileManager.default.fileExists(atPath: path) {
                NSWorkspace.shared.selectFile(path, inFileViewerRootedAtPath: "")
            } else {
                let parentDir = URL(fileURLWithPath: path).deletingLastPathComponent().path
                NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: parentDir)
            }
        }
    }

    @objc private func doneClicked() {
        if let parent = parentSheetWindow, parent.attachedSheet == window {
            parent.endSheet(window!, returnCode: .OK)
        } else {
            window?.close()
        }
        if VMCommandsSheetController.current === self {
            VMCommandsSheetController.current = nil
        }
    }

    // MARK: - Accessibility Adaptations

    @objc private func accessibilityDisplayOptionsDidChange(_ notification: Notification) {
        updateAccessibilityPreferences()
    }

    private func updateAccessibilityPreferences() {
        let reduceTransparency = CubeTokens.reduceTransparency
        guard let window = self.window else { return }

        window.isOpaque = true
        window.backgroundColor = .windowBackgroundColor

        rootVisualEffect.material = reduceTransparency ? .windowBackground : .underWindowBackground
        rootVisualEffect.blendingMode = reduceTransparency ? .withinWindow : .behindWindow
        rootVisualEffect.state = reduceTransparency ? .inactive : .active

        bottomBarView.layer?.backgroundColor = CubeTokens.statusBarBackground.cgColor

        refreshCardLayers(in: window.contentView)
        window.contentView?.needsDisplay = true
    }

    private func refreshCardLayers(in view: NSView?) {
        guard let view = view else { return }
        if let card = view as? SheetCardView {
            card.updateColors()
        }
        for sub in view.subviews {
            refreshCardLayers(in: sub)
        }
    }
}
