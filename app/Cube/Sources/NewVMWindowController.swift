import Foundation
import AppKit
import UniformTypeIdentifiers

private class NewVMContentView: NSView {
    override var isFlipped: Bool { return true }
}

private class NewVMCardView: NSView {
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

private struct OSSelectionOption {
    let title: String
    let osVersion: String
    let customIsoPath: String?
    let isCustomFilePicker: Bool
}

class NewVMWindowController: NSWindowController, NSWindowDelegate {
    static var shared: NewVMWindowController?

    // UI Architecture
    private var rootVisualEffect: NSVisualEffectView!
    private var formScrollView: NSScrollView!
    private var stackView: NSStackView!
    private var logCardView: NewVMCardView!

    // Form Controls
    private var nameField: NSTextField!
    private var osPopUp: NSPopUpButton!
    private var isoPathField: NSTextField!
    private var browseButton: NSButton!
    private var unattendedCheckbox: NSButton!
    private var desktopPopUp: NSPopUpButton!
    private var cpuPopUp: NSPopUpButton!
    private var ramPopUp: NSPopUpButton!
    private var memPreallocCheckbox: NSButton!
    private var diskField: NSTextField!
    private var passwordField: NSTextField!
    private var securePasswordField: NSSecureTextField!
    private var visiblePasswordField: NSTextField!
    private var passwordVisibilityButton: NSButton!
    private var netPopUp: NSPopUpButton!
    private var netHintLabel: NSTextField!
    private var sshPortField: NSTextField!
    private var vncPortField: NSTextField!

    // Shared Folder
    private var sharePathField: NSTextField!
    private var shareBrowseButton: NSButton!
    private var shareTagField: NSTextField!
    private var shareReadOnlyCheckbox: NSButton!

    // Status / Progress / Logging
    private var statusBarView: NSView!
    private var progressIndicator: NSProgressIndicator!
    private var statusLabel: NSTextField!
    private var logTextView: NSTextView!
    private var logScrollView: NSScrollView!
    private var copyLogsButton: NSButton!
    private var createButton: NSButton!
    private var cancelButton: NSButton!

    // Storage
    private var storagePathLabel: NSTextField!
    private var customizeStorageButton: NSButton!
    private var customStorageDir: String? = nil

    private var isBusy = false

    deinit {
        NotificationCenter.default.removeObserver(self)
        NSWorkspace.shared.notificationCenter.removeObserver(self)
    }

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 560, height: 600),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "New Virtual Machine"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isOpaque = false
        window.backgroundColor = .clear
        window.minSize = NSSize(width: 500, height: 440)
        window.center()
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self

        setupUI()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func show() {
        AppDelegate.ensureRegularActivation()
        if shared == nil {
            shared = NewVMWindowController()
        }
        shared?.resetUI()
        shared?.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowWillClose(_ notification: Notification) {
        let hasOtherWindows = PreferencesWindowController.shared.window?.isVisible == true ||
                              DashboardWindowController.shared.window?.isVisible == true
        if !hasOtherWindows {
            NSApp.setActivationPolicy(.accessory)
        }
    }

    private func resetUI() {
        nameField.stringValue = "freebsd-\(Int.random(in: 100...999))"
        populateOSOptions()
        osPopUp.selectItem(at: 0)
        osSelectionChanged()
        desktopPopUp.selectItem(at: 0)
        cpuPopUp.selectItem(at: 1) // 2 vCPUs
        ramPopUp.selectItem(at: 1) // 2048 MB
        memPreallocCheckbox?.state = .off
        diskField.stringValue = "20"
        securePasswordField.stringValue = "root"
        visiblePasswordField.stringValue = "root"
        securePasswordField.isHidden = false
        visiblePasswordField.isHidden = true
        passwordField = securePasswordField
        passwordVisibilityButton.title = "Show"
        setupNetPopUp()
        sshPortField.stringValue = "\(VMManager.shared.nextAvailableSSHPort())"
        vncPortField.stringValue = "\(VMManager.shared.nextAvailableVNCPort())"
        statusLabel.stringValue = "Ready"
        progressIndicator.stopAnimation(nil)
        progressIndicator.isHidden = true
        logTextView.string = ""
        logCardView.isHidden = true
        copyLogsButton.isHidden = true
        customStorageDir = nil
        updateStoragePathDisplay()
        sharePathField?.stringValue = ""
        shareTagField?.stringValue = "shared"
        shareReadOnlyCheckbox?.state = .off
        setFormEnabled(true)
    }

    // MARK: - UI Construction (Auto Layout & Translucent Frosted Glass)

    private func setupUI() {
        guard let window = self.window else { return }

        // Root Liquid Translucent Glass (underWindowBackground blur behind window)
        rootVisualEffect = NSVisualEffectView(frame: window.contentView?.bounds ?? NSRect(x: 0, y: 0, width: 560, height: 740))
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

        // Top Header
        let header = NSView()
        header.translatesAutoresizingMaskIntoConstraints = false
        rootVisualEffect.addSubview(header)

        let titleLabel = NSTextField(labelWithString: "Create Virtual Machine")
        titleLabel.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.font = NSFont.boldSystemFont(ofSize: 16)
        titleLabel.textColor = CubeTokens.primaryText
        header.addSubview(titleLabel)

        let subtitleLabel = NSTextField(labelWithString: "Configure and provision an ARM64 guest system using Cube.")
        subtitleLabel.translatesAutoresizingMaskIntoConstraints = false
        subtitleLabel.font = NSFont.systemFont(ofSize: 11.5)
        subtitleLabel.textColor = CubeTokens.secondaryText
        header.addSubview(subtitleLabel)

        let sepHeader = NSBox()
        sepHeader.translatesAutoresizingMaskIntoConstraints = false
        sepHeader.boxType = .custom
        sepHeader.borderWidth = 0
        sepHeader.fillColor = CubeTokens.separatorColor
        header.addSubview(sepHeader)

        // Bottom Action & Status Bar
        statusBarView = NSView()
        statusBarView.translatesAutoresizingMaskIntoConstraints = false
        statusBarView.wantsLayer = true
        statusBarView.layer?.backgroundColor = CubeTokens.statusBarBackground.cgColor
        rootVisualEffect.addSubview(statusBarView)

        let sepBottom = NSBox()
        sepBottom.translatesAutoresizingMaskIntoConstraints = false
        sepBottom.boxType = .custom
        sepBottom.borderWidth = 0
        sepBottom.fillColor = CubeTokens.separatorColor
        statusBarView.addSubview(sepBottom)

        progressIndicator = NSProgressIndicator()
        progressIndicator.translatesAutoresizingMaskIntoConstraints = false
        progressIndicator.isIndeterminate = true
        progressIndicator.style = .spinning
        progressIndicator.controlSize = .small
        progressIndicator.isHidden = true
        statusBarView.addSubview(progressIndicator)

        statusLabel = NSTextField(labelWithString: "Ready")
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        statusLabel.font = NSFont.systemFont(ofSize: 11.5, weight: .medium)
        statusLabel.textColor = CubeTokens.secondaryText
        statusLabel.lineBreakMode = .byTruncatingTail
        statusBarView.addSubview(statusLabel)

        copyLogsButton = NSButton(title: "Copy Logs", target: self, action: #selector(copyLogsClicked))
        copyLogsButton.translatesAutoresizingMaskIntoConstraints = false
        copyLogsButton.bezelStyle = .rounded
        copyLogsButton.font = NSFont.systemFont(ofSize: 12)
        copyLogsButton.isHidden = true
        statusBarView.addSubview(copyLogsButton)

        cancelButton = NSButton(title: "Cancel", target: self, action: #selector(cancelClicked))
        cancelButton.translatesAutoresizingMaskIntoConstraints = false
        cancelButton.bezelStyle = .rounded
        cancelButton.font = NSFont.systemFont(ofSize: 12)
        statusBarView.addSubview(cancelButton)

        createButton = NSButton(title: "Create VM", target: self, action: #selector(createClicked))
        createButton.translatesAutoresizingMaskIntoConstraints = false
        createButton.bezelStyle = .rounded
        createButton.keyEquivalent = "\r"
        createButton.font = NSFont.boldSystemFont(ofSize: 12)
        statusBarView.addSubview(createButton)

        // Middle Scroll View
        formScrollView = NSScrollView()
        formScrollView.translatesAutoresizingMaskIntoConstraints = false
        formScrollView.drawsBackground = false
        formScrollView.hasVerticalScroller = true
        formScrollView.autohidesScrollers = true
        formScrollView.automaticallyAdjustsContentInsets = false
        rootVisualEffect.addSubview(formScrollView)

        let scrollContentView = NewVMContentView()
        scrollContentView.translatesAutoresizingMaskIntoConstraints = false
        formScrollView.documentView = scrollContentView

        stackView = NSStackView()
        stackView.translatesAutoresizingMaskIntoConstraints = false
        stackView.orientation = .vertical
        stackView.alignment = .leading
        stackView.distribution = .fill
        stackView.spacing = 14
        scrollContentView.addSubview(stackView)

        NSLayoutConstraint.activate([
            // Header Layout (height 76 clears top traffic lights)
            header.topAnchor.constraint(equalTo: rootVisualEffect.topAnchor),
            header.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            header.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            header.heightAnchor.constraint(equalToConstant: 76),

            titleLabel.leadingAnchor.constraint(equalTo: header.leadingAnchor, constant: 20),
            titleLabel.topAnchor.constraint(equalTo: header.topAnchor, constant: 26),
            titleLabel.trailingAnchor.constraint(equalTo: header.trailingAnchor, constant: -20),

            subtitleLabel.leadingAnchor.constraint(equalTo: titleLabel.leadingAnchor),
            subtitleLabel.topAnchor.constraint(equalTo: titleLabel.bottomAnchor, constant: 3),
            subtitleLabel.trailingAnchor.constraint(equalTo: header.trailingAnchor, constant: -20),

            sepHeader.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            sepHeader.trailingAnchor.constraint(equalTo: header.trailingAnchor),
            sepHeader.bottomAnchor.constraint(equalTo: header.bottomAnchor),
            sepHeader.heightAnchor.constraint(equalToConstant: 1),

            // Status Bar Layout
            statusBarView.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            statusBarView.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            statusBarView.bottomAnchor.constraint(equalTo: rootVisualEffect.bottomAnchor),
            statusBarView.heightAnchor.constraint(equalToConstant: 52),

            sepBottom.topAnchor.constraint(equalTo: statusBarView.topAnchor),
            sepBottom.leadingAnchor.constraint(equalTo: statusBarView.leadingAnchor),
            sepBottom.trailingAnchor.constraint(equalTo: statusBarView.trailingAnchor),
            sepBottom.heightAnchor.constraint(equalToConstant: 1),

            progressIndicator.leadingAnchor.constraint(equalTo: statusBarView.leadingAnchor, constant: 18),
            progressIndicator.centerYAnchor.constraint(equalTo: statusBarView.centerYAnchor),
            progressIndicator.widthAnchor.constraint(equalToConstant: 16),
            progressIndicator.heightAnchor.constraint(equalToConstant: 16),

            statusLabel.leadingAnchor.constraint(equalTo: progressIndicator.trailingAnchor, constant: 8),
            statusLabel.centerYAnchor.constraint(equalTo: statusBarView.centerYAnchor),
            statusLabel.trailingAnchor.constraint(lessThanOrEqualTo: copyLogsButton.leadingAnchor, constant: -12),

            createButton.trailingAnchor.constraint(equalTo: statusBarView.trailingAnchor, constant: -18),
            createButton.centerYAnchor.constraint(equalTo: statusBarView.centerYAnchor),
            createButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 88),

            cancelButton.trailingAnchor.constraint(equalTo: createButton.leadingAnchor, constant: -8),
            cancelButton.centerYAnchor.constraint(equalTo: statusBarView.centerYAnchor),

            copyLogsButton.trailingAnchor.constraint(equalTo: cancelButton.leadingAnchor, constant: -8),
            copyLogsButton.centerYAnchor.constraint(equalTo: statusBarView.centerYAnchor),

            // Scroll View Layout
            formScrollView.topAnchor.constraint(equalTo: header.bottomAnchor),
            formScrollView.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            formScrollView.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            formScrollView.bottomAnchor.constraint(equalTo: statusBarView.topAnchor),

            scrollContentView.topAnchor.constraint(equalTo: formScrollView.contentView.topAnchor),
            scrollContentView.leadingAnchor.constraint(equalTo: formScrollView.contentView.leadingAnchor),
            scrollContentView.trailingAnchor.constraint(equalTo: formScrollView.contentView.trailingAnchor),
            scrollContentView.widthAnchor.constraint(equalTo: formScrollView.contentView.widthAnchor),

            stackView.topAnchor.constraint(equalTo: scrollContentView.topAnchor, constant: 14),
            stackView.leadingAnchor.constraint(equalTo: scrollContentView.leadingAnchor, constant: 18),
            stackView.trailingAnchor.constraint(equalTo: scrollContentView.trailingAnchor, constant: -18),
            stackView.bottomAnchor.constraint(equalTo: scrollContentView.bottomAnchor, constant: -18)
        ])

        // Build Cards
        buildIdentityCard()
        buildHardwareCard()
        buildNetworkCard()
        buildSharesCard()
        buildLogCard()

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

    private func addFormRow(
        label: String,
        control: NSView,
        in card: NSView,
        topAnchor: NSLayoutYAxisAnchor,
        topOffset: CGFloat = 10,
        height: CGFloat? = nil,
        baselineView: NSView? = nil
    ) -> NSLayoutYAxisAnchor {
        let labelField = NSTextField(labelWithString: label)
        labelField.translatesAutoresizingMaskIntoConstraints = false
        labelField.font = NSFont.systemFont(ofSize: 11.5, weight: .medium)
        labelField.textColor = CubeTokens.secondaryText
        labelField.alignment = .right
        card.addSubview(labelField)

        control.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(control)

        var constraints = [
            labelField.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            labelField.widthAnchor.constraint(equalToConstant: 120),
            labelField.firstBaselineAnchor.constraint(equalTo: (baselineView ?? control).firstBaselineAnchor),

            control.leadingAnchor.constraint(equalTo: labelField.trailingAnchor, constant: 12),
            control.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            control.topAnchor.constraint(equalTo: topAnchor, constant: topOffset)
        ]
        if let h = height {
            constraints.append(control.heightAnchor.constraint(equalToConstant: h))
        }
        NSLayoutConstraint.activate(constraints)
        return control.bottomAnchor
    }

    // MARK: - Section 1: Machine Identity & OS

    private func buildIdentityCard() {
        let card = NewVMCardView()
        stackView.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: stackView.widthAnchor).isActive = true

        var lastAnchor = addCardHeader(title: "Machine Identity & OS", to: card)

        // VM Name
        nameField = NSTextField()
        nameField.font = NSFont.systemFont(ofSize: 12)
        nameField.placeholderString = "e.g. freebsd-work"
        nameField.toolTip = "Unique name identifying this virtual machine in Cube.app and cubectl CLI."
        NotificationCenter.default.addObserver(self, selector: #selector(nameFieldChanged), name: NSControl.textDidChangeNotification, object: nameField)
        lastAnchor = addFormRow(label: "VM Name:", control: nameField, in: card, topAnchor: lastAnchor, height: 22)

        // Operating System
        osPopUp = NSPopUpButton(frame: .zero, pullsDown: false)
        osPopUp.font = NSFont.systemFont(ofSize: 12)
        osPopUp.toolTip = "Choose an official FreeBSD ARM64 release (auto-downloaded and verified) or provide a custom boot ISO."
        osPopUp.target = self
        osPopUp.action = #selector(osSelectionChanged)
        populateOSOptions()
        lastAnchor = addFormRow(label: "Operating System:", control: osPopUp, in: card, topAnchor: lastAnchor, height: 24)

        // Custom ISO Path Row
        let isoRow = NSView()
        isoRow.translatesAutoresizingMaskIntoConstraints = false

        isoPathField = NSTextField()
        isoPathField.translatesAutoresizingMaskIntoConstraints = false
        isoPathField.font = NSFont.systemFont(ofSize: 11)
        isoPathField.placeholderString = "Select an .iso image"
        isoPathField.toolTip = "Path to a local bootable installer ISO image on your Mac."
        isoPathField.isEnabled = false
        isoRow.addSubview(isoPathField)

        browseButton = NSButton(title: "Browse...", target: self, action: #selector(browseIsoClicked))
        browseButton.translatesAutoresizingMaskIntoConstraints = false
        browseButton.bezelStyle = .rounded
        browseButton.font = NSFont.systemFont(ofSize: 11)
        browseButton.isEnabled = false
        isoRow.addSubview(browseButton)

        NSLayoutConstraint.activate([
            isoPathField.leadingAnchor.constraint(equalTo: isoRow.leadingAnchor),
            isoPathField.centerYAnchor.constraint(equalTo: isoRow.centerYAnchor),
            isoPathField.heightAnchor.constraint(equalToConstant: 22),

            browseButton.leadingAnchor.constraint(equalTo: isoPathField.trailingAnchor, constant: 8),
            browseButton.trailingAnchor.constraint(equalTo: isoRow.trailingAnchor),
            browseButton.centerYAnchor.constraint(equalTo: isoRow.centerYAnchor),
            browseButton.widthAnchor.constraint(equalToConstant: 80)
        ])
        lastAnchor = addFormRow(
            label: "Custom ISO:", control: isoRow, in: card, topAnchor: lastAnchor,
            height: 24, baselineView: isoPathField
        )

        // Unattended checkbox & hint
        let unattendedContainer = NSView()
        unattendedContainer.translatesAutoresizingMaskIntoConstraints = false

        unattendedCheckbox = NSButton(checkboxWithTitle: "Run Automated Unattended Installation (Zero Prompts)", target: self, action: #selector(unattendedChanged))
        unattendedCheckbox.translatesAutoresizingMaskIntoConstraints = false
        unattendedCheckbox.state = .on
        unattendedCheckbox.font = NSFont.systemFont(ofSize: 12)
        unattendedCheckbox.toolTip = "Automates disk partitioning (ZFS), root user account setup, and SSH service configuration with zero installer prompts."
        unattendedContainer.addSubview(unattendedCheckbox)

        let unattendedHint = NSTextField(labelWithString: "💡 Auto-provisions ZFS root filesystem, accounts, and SSH automatically.")
        unattendedHint.translatesAutoresizingMaskIntoConstraints = false
        unattendedHint.font = NSFont.systemFont(ofSize: 10.5)
        unattendedHint.textColor = CubeTokens.secondaryText
        unattendedContainer.addSubview(unattendedHint)

        NSLayoutConstraint.activate([
            unattendedCheckbox.topAnchor.constraint(equalTo: unattendedContainer.topAnchor),
            unattendedCheckbox.leadingAnchor.constraint(equalTo: unattendedContainer.leadingAnchor),
            unattendedCheckbox.trailingAnchor.constraint(equalTo: unattendedContainer.trailingAnchor),

            unattendedHint.topAnchor.constraint(equalTo: unattendedCheckbox.bottomAnchor, constant: 4),
            unattendedHint.leadingAnchor.constraint(equalTo: unattendedCheckbox.leadingAnchor, constant: 18),
            unattendedHint.trailingAnchor.constraint(equalTo: unattendedContainer.trailingAnchor),
            unattendedHint.bottomAnchor.constraint(equalTo: unattendedContainer.bottomAnchor)
        ])

        let unattendedLabel = NSTextField(labelWithString: "Automation:")
        unattendedLabel.translatesAutoresizingMaskIntoConstraints = false
        unattendedLabel.font = NSFont.systemFont(ofSize: 11.5, weight: .medium)
        unattendedLabel.textColor = CubeTokens.secondaryText
        unattendedLabel.alignment = .right
        card.addSubview(unattendedLabel)
        card.addSubview(unattendedContainer)

        NSLayoutConstraint.activate([
            unattendedLabel.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            unattendedLabel.widthAnchor.constraint(equalToConstant: 120),
            unattendedLabel.topAnchor.constraint(equalTo: lastAnchor, constant: 12),

            unattendedContainer.leadingAnchor.constraint(equalTo: unattendedLabel.trailingAnchor, constant: 12),
            unattendedContainer.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            unattendedContainer.topAnchor.constraint(equalTo: unattendedLabel.topAnchor),
        ])

        let desktopLabel = NSTextField(labelWithString: "Desktop:")
        desktopLabel.translatesAutoresizingMaskIntoConstraints = false
        desktopLabel.font = NSFont.systemFont(ofSize: 11.5, weight: .medium)
        desktopLabel.textColor = CubeTokens.secondaryText
        desktopLabel.alignment = .right
        card.addSubview(desktopLabel)

        desktopPopUp = NSPopUpButton(frame: .zero, pullsDown: false)
        desktopPopUp.translatesAutoresizingMaskIntoConstraints = false
        desktopPopUp.font = NSFont.systemFont(ofSize: 12)
        desktopPopUp.addItems(withTitles: ["None (Server)", "XFCE", "KDE Plasma"])
        desktopPopUp.item(at: 0)?.representedObject = ""
        desktopPopUp.item(at: 1)?.representedObject = "xfce"
        desktopPopUp.item(at: 2)?.representedObject = "plasma"
        desktopPopUp.target = self
        desktopPopUp.action = #selector(desktopChanged)
        desktopPopUp.toolTip = "Install a graphical desktop after the unattended base-system installation."
        card.addSubview(desktopPopUp)

        NSLayoutConstraint.activate([
            desktopLabel.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            desktopLabel.widthAnchor.constraint(equalToConstant: 120),
            desktopLabel.centerYAnchor.constraint(equalTo: desktopPopUp.centerYAnchor),

            desktopPopUp.topAnchor.constraint(equalTo: unattendedContainer.bottomAnchor, constant: 12),
            desktopPopUp.leadingAnchor.constraint(equalTo: desktopLabel.trailingAnchor, constant: 12),
            desktopPopUp.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            desktopPopUp.heightAnchor.constraint(equalToConstant: 24),
            desktopPopUp.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -14)
        ])
    }

    // MARK: - Section 2: Hardware Configuration

    private func buildHardwareCard() {
        let card = NewVMCardView()
        stackView.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: stackView.widthAnchor).isActive = true

        var lastAnchor = addCardHeader(title: "Hardware Configuration", to: card)

        // Virtual CPUs
        cpuPopUp = NSPopUpButton(frame: .zero, pullsDown: false)
        cpuPopUp.font = NSFont.systemFont(ofSize: 12)
        cpuPopUp.addItems(withTitles: ["1 Core", "2 Cores", "4 Cores", "8 Cores"])
        cpuPopUp.selectItem(at: 1)
        cpuPopUp.toolTip = "Number of virtual CPU cores allocated to the guest VM (1 to 8 cores)."
        lastAnchor = addFormRow(label: "Virtual CPUs:", control: cpuPopUp, in: card, topAnchor: lastAnchor, height: 24)

        // Memory (RAM) + Preallocate checkbox
        let ramRow = NSView()
        ramRow.translatesAutoresizingMaskIntoConstraints = false

        ramPopUp = NSPopUpButton(frame: .zero, pullsDown: false)
        ramPopUp.translatesAutoresizingMaskIntoConstraints = false
        ramPopUp.font = NSFont.systemFont(ofSize: 12)
        ramPopUp.addItems(withTitles: ["1024 MB (1 GB)", "2048 MB (2 GB)", "4096 MB (4 GB)", "8192 MB (8 GB)", "16384 MB (16 GB)"])
        ramPopUp.selectItem(at: 1)
        ramPopUp.toolTip = "Guest physical memory size. By default, host memory is allocated on-demand as guest pages are used."
        ramRow.addSubview(ramPopUp)

        memPreallocCheckbox = NSButton(checkboxWithTitle: "Pre-allocate RAM", target: nil, action: nil)
        memPreallocCheckbox.translatesAutoresizingMaskIntoConstraints = false
        memPreallocCheckbox.font = NSFont.systemFont(ofSize: 11)
        memPreallocCheckbox.toolTip = "Pre-faults all RAM upfront on launch (disables lazy on-demand paging)"
        ramRow.addSubview(memPreallocCheckbox)

        NSLayoutConstraint.activate([
            ramPopUp.leadingAnchor.constraint(equalTo: ramRow.leadingAnchor),
            ramPopUp.centerYAnchor.constraint(equalTo: ramRow.centerYAnchor),
            ramPopUp.widthAnchor.constraint(equalToConstant: 140),

            memPreallocCheckbox.leadingAnchor.constraint(equalTo: ramPopUp.trailingAnchor, constant: 14),
            memPreallocCheckbox.trailingAnchor.constraint(lessThanOrEqualTo: ramRow.trailingAnchor),
            memPreallocCheckbox.centerYAnchor.constraint(equalTo: ramRow.centerYAnchor)
        ])
        lastAnchor = addFormRow(
            label: "Memory (RAM):", control: ramRow, in: card, topAnchor: lastAnchor,
            height: 24, baselineView: ramPopUp
        )

        // Virtual Disk Size
        let diskRow = NSView()
        diskRow.translatesAutoresizingMaskIntoConstraints = false

        diskField = NSTextField()
        diskField.translatesAutoresizingMaskIntoConstraints = false
        diskField.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        diskField.stringValue = "20"
        diskField.alignment = .right
        diskField.toolTip = "Virtual disk capacity in GB. Disks are created as sparse files, only consuming disk space as data is written."
        diskRow.addSubview(diskField)

        let gbLabel = NSTextField(labelWithString: "GB (sparse ZFS disk image)")
        gbLabel.translatesAutoresizingMaskIntoConstraints = false
        gbLabel.font = NSFont.systemFont(ofSize: 11)
        gbLabel.textColor = CubeTokens.secondaryText
        diskRow.addSubview(gbLabel)

        NSLayoutConstraint.activate([
            diskField.leadingAnchor.constraint(equalTo: diskRow.leadingAnchor),
            diskField.centerYAnchor.constraint(equalTo: diskRow.centerYAnchor),
            diskField.widthAnchor.constraint(equalToConstant: 64),
            diskField.heightAnchor.constraint(equalToConstant: 22),

            gbLabel.leadingAnchor.constraint(equalTo: diskField.trailingAnchor, constant: 8),
            gbLabel.trailingAnchor.constraint(lessThanOrEqualTo: diskRow.trailingAnchor),
            gbLabel.centerYAnchor.constraint(equalTo: diskRow.centerYAnchor)
        ])
        lastAnchor = addFormRow(
            label: "Virtual Disk Size:", control: diskRow, in: card, topAnchor: lastAnchor,
            height: 24, baselineView: diskField
        )

        // Storage Location
        let storageRow = NSView()
        storageRow.translatesAutoresizingMaskIntoConstraints = false

        storagePathLabel = NSTextField(labelWithString: "")
        storagePathLabel.translatesAutoresizingMaskIntoConstraints = false
        storagePathLabel.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        storagePathLabel.lineBreakMode = .byTruncatingMiddle
        storageRow.addSubview(storagePathLabel)

        customizeStorageButton = NSButton(title: "Customize...", target: self, action: #selector(customizeStorageClicked))
        customizeStorageButton.translatesAutoresizingMaskIntoConstraints = false
        customizeStorageButton.bezelStyle = .rounded
        customizeStorageButton.font = NSFont.systemFont(ofSize: 11)
        storageRow.addSubview(customizeStorageButton)

        NSLayoutConstraint.activate([
            storagePathLabel.leadingAnchor.constraint(equalTo: storageRow.leadingAnchor),
            storagePathLabel.centerYAnchor.constraint(equalTo: storageRow.centerYAnchor),

            customizeStorageButton.leadingAnchor.constraint(equalTo: storagePathLabel.trailingAnchor, constant: 8),
            customizeStorageButton.trailingAnchor.constraint(equalTo: storageRow.trailingAnchor),
            customizeStorageButton.centerYAnchor.constraint(equalTo: storageRow.centerYAnchor),
            customizeStorageButton.widthAnchor.constraint(equalToConstant: 88)
        ])
        lastAnchor = addFormRow(
            label: "Storage Location:", control: storageRow, in: card, topAnchor: lastAnchor,
            height: 24, baselineView: storagePathLabel
        )

        card.bottomAnchor.constraint(equalTo: lastAnchor, constant: 14).isActive = true
        updateStoragePathDisplay()
    }

    // MARK: - Section 3: Networking & Security

    private func buildNetworkCard() {
        let card = NewVMCardView()
        stackView.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: stackView.widthAnchor).isActive = true

        var lastAnchor = addCardHeader(title: "Networking & Security", to: card)

        // Root Password
        let passwordRow = NSView()
        passwordRow.translatesAutoresizingMaskIntoConstraints = false

        securePasswordField = NSSecureTextField()
        securePasswordField.translatesAutoresizingMaskIntoConstraints = false
        securePasswordField.identifier = NSUserInterfaceItemIdentifier("rootPasswordSecureField")
        securePasswordField.font = NSFont.systemFont(ofSize: 12)
        securePasswordField.stringValue = "root"
        securePasswordField.toolTip = "Root password for guest console login and SSH connections (default: root)."
        passwordRow.addSubview(securePasswordField)

        visiblePasswordField = NSTextField()
        visiblePasswordField.translatesAutoresizingMaskIntoConstraints = false
        visiblePasswordField.identifier = NSUserInterfaceItemIdentifier("rootPasswordVisibleField")
        visiblePasswordField.font = NSFont.systemFont(ofSize: 12)
        visiblePasswordField.stringValue = "root"
        visiblePasswordField.toolTip = securePasswordField.toolTip
        visiblePasswordField.isHidden = true
        passwordRow.addSubview(visiblePasswordField)

        passwordVisibilityButton = NSButton(title: "Show", target: self, action: #selector(togglePasswordVisibility))
        passwordVisibilityButton.translatesAutoresizingMaskIntoConstraints = false
        passwordVisibilityButton.identifier = NSUserInterfaceItemIdentifier("rootPasswordVisibilityButton")
        passwordVisibilityButton.bezelStyle = .rounded
        passwordVisibilityButton.font = NSFont.systemFont(ofSize: 11)
        passwordVisibilityButton.toolTip = "Show or hide the root password."
        passwordRow.addSubview(passwordVisibilityButton)

        NSLayoutConstraint.activate([
            securePasswordField.leadingAnchor.constraint(equalTo: passwordRow.leadingAnchor),
            securePasswordField.topAnchor.constraint(equalTo: passwordRow.topAnchor),
            securePasswordField.bottomAnchor.constraint(equalTo: passwordRow.bottomAnchor),
            securePasswordField.trailingAnchor.constraint(equalTo: passwordVisibilityButton.leadingAnchor, constant: -8),

            visiblePasswordField.leadingAnchor.constraint(equalTo: securePasswordField.leadingAnchor),
            visiblePasswordField.trailingAnchor.constraint(equalTo: securePasswordField.trailingAnchor),
            visiblePasswordField.topAnchor.constraint(equalTo: securePasswordField.topAnchor),
            visiblePasswordField.bottomAnchor.constraint(equalTo: securePasswordField.bottomAnchor),

            passwordVisibilityButton.trailingAnchor.constraint(equalTo: passwordRow.trailingAnchor),
            passwordVisibilityButton.centerYAnchor.constraint(equalTo: passwordRow.centerYAnchor),
            passwordVisibilityButton.widthAnchor.constraint(equalToConstant: 58)
        ])
        passwordField = securePasswordField
        lastAnchor = addFormRow(
            label: "Root Password:",
            control: passwordRow,
            in: card,
            topAnchor: lastAnchor,
            height: 22,
            baselineView: securePasswordField
        )

        // Network Mode
        let netContainer = NSView()
        netContainer.translatesAutoresizingMaskIntoConstraints = false

        netPopUp = NSPopUpButton(frame: .zero, pullsDown: false)
        netPopUp.translatesAutoresizingMaskIntoConstraints = false
        netPopUp.font = NSFont.systemFont(ofSize: 12)
        netContainer.addSubview(netPopUp)

        netHintLabel = NSTextField(labelWithString: "")
        netHintLabel.translatesAutoresizingMaskIntoConstraints = false
        netHintLabel.font = NSFont.systemFont(ofSize: 10)
        netHintLabel.textColor = CubeTokens.secondaryText
        netContainer.addSubview(netHintLabel)

        setupNetPopUp()

        NSLayoutConstraint.activate([
            netPopUp.topAnchor.constraint(equalTo: netContainer.topAnchor),
            netPopUp.leadingAnchor.constraint(equalTo: netContainer.leadingAnchor),
            netPopUp.trailingAnchor.constraint(equalTo: netContainer.trailingAnchor),
            netPopUp.heightAnchor.constraint(equalToConstant: 24),

            netHintLabel.topAnchor.constraint(equalTo: netPopUp.bottomAnchor, constant: 4),
            netHintLabel.leadingAnchor.constraint(equalTo: netContainer.leadingAnchor, constant: 2),
            netHintLabel.trailingAnchor.constraint(equalTo: netContainer.trailingAnchor),
            netHintLabel.bottomAnchor.constraint(equalTo: netContainer.bottomAnchor)
        ])
        lastAnchor = addFormRow(
            label: "Network Mode:", control: netContainer, in: card, topAnchor: lastAnchor,
            baselineView: netPopUp
        )

        // Forwarded Ports Row (SSH and VNC)
        let portsRow = NSView()
        portsRow.translatesAutoresizingMaskIntoConstraints = false

        let sshLabel = NSTextField(labelWithString: "SSH:")
        sshLabel.translatesAutoresizingMaskIntoConstraints = false
        sshLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        sshLabel.textColor = CubeTokens.secondaryText
        portsRow.addSubview(sshLabel)

        sshPortField = NSTextField()
        sshPortField.translatesAutoresizingMaskIntoConstraints = false
        sshPortField.font = NSFont.monospacedSystemFont(ofSize: 11.5, weight: .regular)
        sshPortField.stringValue = "\(VMManager.shared.nextAvailableSSHPort())"
        sshPortField.toolTip = "Host port forwarded to guest SSH (port 22). Connect with: ssh root@127.0.0.1 -p <port>"
        portsRow.addSubview(sshPortField)

        let vncLabel = NSTextField(labelWithString: "VNC:")
        vncLabel.translatesAutoresizingMaskIntoConstraints = false
        vncLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        vncLabel.textColor = CubeTokens.secondaryText
        portsRow.addSubview(vncLabel)

        vncPortField = NSTextField()
        vncPortField.translatesAutoresizingMaskIntoConstraints = false
        vncPortField.font = NSFont.monospacedSystemFont(ofSize: 11.5, weight: .regular)
        vncPortField.stringValue = "\(VMManager.shared.nextAvailableVNCPort())"
        vncPortField.toolTip = "Host port for the VNC display (port 5900+). Cube.app opens it with TigerVNC."
        portsRow.addSubview(vncPortField)

        NSLayoutConstraint.activate([
            sshLabel.leadingAnchor.constraint(equalTo: portsRow.leadingAnchor),
            sshLabel.centerYAnchor.constraint(equalTo: portsRow.centerYAnchor),

            sshPortField.leadingAnchor.constraint(equalTo: sshLabel.trailingAnchor, constant: 6),
            sshPortField.centerYAnchor.constraint(equalTo: portsRow.centerYAnchor),
            sshPortField.widthAnchor.constraint(equalToConstant: 64),
            sshPortField.heightAnchor.constraint(equalToConstant: 22),

            vncLabel.leadingAnchor.constraint(equalTo: sshPortField.trailingAnchor, constant: 20),
            vncLabel.centerYAnchor.constraint(equalTo: portsRow.centerYAnchor),

            vncPortField.leadingAnchor.constraint(equalTo: vncLabel.trailingAnchor, constant: 6),
            vncPortField.centerYAnchor.constraint(equalTo: portsRow.centerYAnchor),
            vncPortField.widthAnchor.constraint(equalToConstant: 64),
            vncPortField.heightAnchor.constraint(equalToConstant: 22),
            vncPortField.trailingAnchor.constraint(lessThanOrEqualTo: portsRow.trailingAnchor)
        ])
        lastAnchor = addFormRow(
            label: "Network Ports:", control: portsRow, in: card, topAnchor: lastAnchor,
            height: 24, baselineView: sshPortField
        )

        card.bottomAnchor.constraint(equalTo: lastAnchor, constant: 14).isActive = true
    }

    // MARK: - Section 4: VirtIO-9P Shared Folder

    private func buildSharesCard() {
        let card = NewVMCardView()
        stackView.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: stackView.widthAnchor).isActive = true

        var lastAnchor = addCardHeader(title: "Shared Folder (VirtIO-9P)", to: card)

        // Host Path Row
        let sharePathRow = NSView()
        sharePathRow.translatesAutoresizingMaskIntoConstraints = false

        sharePathField = NSTextField()
        sharePathField.translatesAutoresizingMaskIntoConstraints = false
        sharePathField.font = NSFont.systemFont(ofSize: 11)
        sharePathField.placeholderString = "Optional host directory (e.g. ~/Dev)"
        sharePathField.isEditable = false
        sharePathField.toolTip = "Mac directory shared into guest via VirtIO-9P. Mount inside guest: mount -t p9fs <tag> /mnt/host"
        sharePathRow.addSubview(sharePathField)

        shareBrowseButton = NSButton(title: "Choose...", target: self, action: #selector(chooseSharedFolderClicked))
        shareBrowseButton.translatesAutoresizingMaskIntoConstraints = false
        shareBrowseButton.bezelStyle = .rounded
        shareBrowseButton.font = NSFont.systemFont(ofSize: 11)
        sharePathRow.addSubview(shareBrowseButton)

        NSLayoutConstraint.activate([
            sharePathField.leadingAnchor.constraint(equalTo: sharePathRow.leadingAnchor),
            sharePathField.centerYAnchor.constraint(equalTo: sharePathRow.centerYAnchor),
            sharePathField.heightAnchor.constraint(equalToConstant: 22),

            shareBrowseButton.leadingAnchor.constraint(equalTo: sharePathField.trailingAnchor, constant: 8),
            shareBrowseButton.trailingAnchor.constraint(equalTo: sharePathRow.trailingAnchor),
            shareBrowseButton.centerYAnchor.constraint(equalTo: sharePathRow.centerYAnchor),
            shareBrowseButton.widthAnchor.constraint(equalToConstant: 80)
        ])
        lastAnchor = addFormRow(
            label: "Host Directory:", control: sharePathRow, in: card, topAnchor: lastAnchor,
            height: 24, baselineView: sharePathField
        )

        // Options Row (Tag, Read-only, Clear)
        let shareOptionsRow = NSView()
        shareOptionsRow.translatesAutoresizingMaskIntoConstraints = false

        let tagLabel = NSTextField(labelWithString: "Tag:")
        tagLabel.translatesAutoresizingMaskIntoConstraints = false
        tagLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        tagLabel.textColor = CubeTokens.secondaryText
        shareOptionsRow.addSubview(tagLabel)

        shareTagField = NSTextField()
        shareTagField.translatesAutoresizingMaskIntoConstraints = false
        shareTagField.stringValue = "shared"
        shareTagField.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        shareTagField.toolTip = "VirtIO-9P mount tag used as the source in the guest mount command."
        shareOptionsRow.addSubview(shareTagField)

        shareReadOnlyCheckbox = NSButton(checkboxWithTitle: "Read-Only", target: nil, action: nil)
        shareReadOnlyCheckbox.translatesAutoresizingMaskIntoConstraints = false
        shareReadOnlyCheckbox.font = NSFont.systemFont(ofSize: 11)
        shareReadOnlyCheckbox.toolTip = "When checked, the guest VM cannot modify or write files to this host directory."
        shareOptionsRow.addSubview(shareReadOnlyCheckbox)

        let clearBtn = NSButton(title: "Clear", target: self, action: #selector(clearSharedFolderClicked))
        clearBtn.translatesAutoresizingMaskIntoConstraints = false
        clearBtn.bezelStyle = .rounded
        clearBtn.font = NSFont.systemFont(ofSize: 11)
        shareOptionsRow.addSubview(clearBtn)

        NSLayoutConstraint.activate([
            tagLabel.leadingAnchor.constraint(equalTo: shareOptionsRow.leadingAnchor),
            tagLabel.centerYAnchor.constraint(equalTo: shareOptionsRow.centerYAnchor),

            shareTagField.leadingAnchor.constraint(equalTo: tagLabel.trailingAnchor, constant: 6),
            shareTagField.centerYAnchor.constraint(equalTo: shareOptionsRow.centerYAnchor),
            shareTagField.widthAnchor.constraint(equalToConstant: 85),
            shareTagField.heightAnchor.constraint(equalToConstant: 22),

            shareReadOnlyCheckbox.leadingAnchor.constraint(equalTo: shareTagField.trailingAnchor, constant: 14),
            shareReadOnlyCheckbox.centerYAnchor.constraint(equalTo: shareOptionsRow.centerYAnchor),

            clearBtn.trailingAnchor.constraint(equalTo: shareOptionsRow.trailingAnchor),
            clearBtn.centerYAnchor.constraint(equalTo: shareOptionsRow.centerYAnchor),
            clearBtn.widthAnchor.constraint(equalToConstant: 60)
        ])
        lastAnchor = addFormRow(
            label: "Share Options:", control: shareOptionsRow, in: card, topAnchor: lastAnchor,
            height: 24, baselineView: shareTagField
        )

        // Mount hint
        let shareHint = NSTextField(labelWithString: "💡 Mount inside guest: mount -t p9fs <tag> /mnt/host")
        shareHint.translatesAutoresizingMaskIntoConstraints = false
        shareHint.font = NSFont.systemFont(ofSize: 10.5)
        shareHint.textColor = CubeTokens.secondaryText
        card.addSubview(shareHint)

        NSLayoutConstraint.activate([
            shareHint.topAnchor.constraint(equalTo: lastAnchor, constant: 6),
            shareHint.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 146),
            shareHint.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            shareHint.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -14)
        ])
    }

    // MARK: - Section 5: Live Creation Logs

    private func buildLogCard() {
        logCardView = NewVMCardView()
        stackView.addArrangedSubview(logCardView)
        logCardView.widthAnchor.constraint(equalTo: stackView.widthAnchor).isActive = true

        let headerAnchor = addCardHeader(title: "Provisioning Logs", to: logCardView)

        logScrollView = NSScrollView()
        logScrollView.translatesAutoresizingMaskIntoConstraints = false
        logScrollView.hasVerticalScroller = true
        logScrollView.borderType = .noBorder
        logScrollView.drawsBackground = false
        logCardView.addSubview(logScrollView)

        logTextView = NSTextView()
        logTextView.isEditable = false
        logTextView.backgroundColor = .clear
        logTextView.font = NSFont.userFixedPitchFont(ofSize: 10)
        logScrollView.documentView = logTextView

        NSLayoutConstraint.activate([
            logScrollView.topAnchor.constraint(equalTo: headerAnchor, constant: 8),
            logScrollView.leadingAnchor.constraint(equalTo: logCardView.leadingAnchor, constant: 14),
            logScrollView.trailingAnchor.constraint(equalTo: logCardView.trailingAnchor, constant: -14),
            logScrollView.heightAnchor.constraint(equalToConstant: 110),
            logScrollView.bottomAnchor.constraint(equalTo: logCardView.bottomAnchor, constant: -14)
        ])

        logCardView.isHidden = true
    }

    func revealProvisioningLogs() {
        logCardView.isHidden = false
        window?.contentView?.layoutSubtreeIfNeeded()
        guard let documentView = formScrollView.documentView else { return }
        let logRect = logCardView.convert(logCardView.bounds, to: documentView)
        documentView.scrollToVisible(logRect)
        formScrollView.reflectScrolledClipView(formScrollView.contentView)
    }

    // MARK: - Popups & Options

    private func setupNetPopUp() {
        netPopUp.removeAllItems()
        let hasEntitlement = VMManager.shared.hasNetworkingEntitlement()

        func addModeItem(title: String, mode: String) {
            netPopUp.addItem(withTitle: title)
            netPopUp.lastItem?.representedObject = mode
        }

        if hasEntitlement {
            addModeItem(title: "Direct vmnet NAT (vmnet) — Default (Entitled ✓)", mode: "vmnet")
            addModeItem(title: "Bridged LAN (bridge) — Entitled ✓", mode: "bridge")
            addModeItem(title: "Offline / Disabled (none)", mode: "none")
            addModeItem(title: "Shared Proxy (vmnet-proxy)", mode: "vmnet-proxy")
            netPopUp.selectItem(at: 0)
            netPopUp.toolTip = "Direct vmnet NAT (vmnet) provides native macOS Hypervisor NAT networking using Apple's vmnet framework."
            netHintLabel?.stringValue = "✓ Entitlement granted: Native Apple vmnet NAT selected by default"
        } else {
            addModeItem(title: "Shared Proxy (vmnet-proxy) — Default (Recommended)", mode: "vmnet-proxy")
            addModeItem(title: "Direct vmnet NAT (vmnet) — Requires Entitlement", mode: "vmnet")
            addModeItem(title: "Bridged LAN (bridge) — Requires Entitlement", mode: "bridge")
            addModeItem(title: "Offline / Disabled (none)", mode: "none")
            netPopUp.selectItem(at: 0)
            netPopUp.toolTip = "Shared Proxy (vmnet-proxy) provides fast user-mode NAT with DHCP (no root/sudo privileges needed)."
            netHintLabel?.stringValue = "💡 Shared Proxy runs in user-mode with DHCP (no root/sudo needed)"
        }
    }

    private func populateOSOptions() {
        osPopUp.removeAllItems()
        let menu = NSMenu()

        let allItems = ImageCacheManager.shared.getAllImageItems()
        let presets = ImageCacheManager.shared.presets

        // 1. Official Presets
        for preset in presets {
            let item = allItems.first(where: { $0.id == preset.id })
            let isCached = item?.isCached == true
            let tag = isCached ? " [Cached]" : " [Auto-download]"
            let title = "\(preset.name)\(tag)"

            let mi = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            mi.representedObject = OSSelectionOption(
                title: title,
                osVersion: preset.version,
                customIsoPath: isCached ? "\(ImageCacheManager.shared.cacheDir)/\(preset.filename)" : nil,
                isCustomFilePicker: false
            )
            menu.addItem(mi)
        }

        // 2. Custom Cached Images in ~/Library/Caches/Cube
        let customCached = allItems.filter { !$0.isPreset && $0.isCached }
        if !customCached.isEmpty {
            menu.addItem(NSMenuItem.separator())
            for img in customCached {
                let fullPath = "\(ImageCacheManager.shared.cacheDir)/\(img.filename)"
                let title = "\(img.title) [Cached ISO]"
                let mi = NSMenuItem(title: title, action: nil, keyEquivalent: "")
                mi.representedObject = OSSelectionOption(
                    title: title,
                    osVersion: "custom",
                    customIsoPath: fullPath,
                    isCustomFilePicker: false
                )
                menu.addItem(mi)
            }
        }

        // 3. Custom ISO browse option
        menu.addItem(NSMenuItem.separator())
        let browseTitle = "Custom Installer ISO (Browse...)"
        let browseMi = NSMenuItem(title: browseTitle, action: nil, keyEquivalent: "")
        browseMi.representedObject = OSSelectionOption(
            title: browseTitle,
            osVersion: "custom",
            customIsoPath: nil,
            isCustomFilePicker: true
        )
        menu.addItem(browseMi)

        osPopUp.menu = menu
    }

    @objc private func osSelectionChanged() {
        guard let opt = osPopUp.selectedItem?.representedObject as? OSSelectionOption else { return }

        isoPathField.isEnabled = opt.isCustomFilePicker
        browseButton.isEnabled = opt.isCustomFilePicker

        if opt.isCustomFilePicker {
            if isoPathField.stringValue.isEmpty {
                browseIsoClicked()
            }
        } else if let cachedPath = opt.customIsoPath {
            isoPathField.stringValue = cachedPath
        } else {
            isoPathField.stringValue = ""
        }

        unattendedCheckbox.isEnabled = true
        unattendedCheckbox.toolTip = "Automates disk partitioning (ZFS), root user account setup, and SSH service configuration with zero installer prompts."
        unattendedChanged()
    }

    @objc private func unattendedChanged() {
        desktopPopUp?.isEnabled = unattendedCheckbox.state == .on && !isBusy
        if unattendedCheckbox.state != .on {
            desktopPopUp?.selectItem(at: 0)
        }
    }

    @objc private func desktopChanged() {
        if desktopPopUp.indexOfSelectedItem > 0 {
            if cpuPopUp.indexOfSelectedItem < 2 { cpuPopUp.selectItem(at: 2) }
            if ramPopUp.indexOfSelectedItem < 2 { ramPopUp.selectItem(at: 2) }
        }
    }

    @objc private func togglePasswordVisibility() {
        let showPassword = visiblePasswordField.isHidden
        let currentValue = passwordField.stringValue
        let wasEditingPassword = passwordField.currentEditor() != nil

        if showPassword {
            visiblePasswordField.stringValue = currentValue
            securePasswordField.isHidden = true
            visiblePasswordField.isHidden = false
            passwordField = visiblePasswordField
            passwordVisibilityButton.title = "Hide"
        } else {
            securePasswordField.stringValue = currentValue
            visiblePasswordField.isHidden = true
            securePasswordField.isHidden = false
            passwordField = securePasswordField
            passwordVisibilityButton.title = "Show"
        }

        if wasEditingPassword {
            window?.makeFirstResponder(passwordField)
            passwordField.currentEditor()?.selectedRange = NSRange(location: currentValue.utf16.count, length: 0)
        }
    }

    @objc private func browseIsoClicked() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if #available(macOS 11.0, *) {
            var types: [UTType] = [.diskImage]
            if let isoType = UTType(filenameExtension: "iso") {
                types.append(isoType)
            }
            if let imgType = UTType(filenameExtension: "img") {
                types.append(imgType)
            }
            panel.allowedContentTypes = types
        }
        panel.beginSheetModal(for: self.window!) { response in
            if response == .OK, let url = panel.url {
                self.isoPathField.stringValue = url.path
            }
        }
    }

    @objc private func cancelClicked() {
        if isBusy {
            let alert = NSAlert()
            alert.messageText = "Creation in Progress"
            alert.informativeText = "A VM is currently being provisioned. Please wait for the process to complete."
            alert.runModal()
            return
        }
        self.window?.close()
    }

    @objc private func createClicked() {
        let name = nameField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty {
            showError("VM Name cannot be empty.")
            return
        }

        let regex = try! NSRegularExpression(pattern: "^[A-Za-z0-9][A-Za-z0-9_-]{0,47}$")
        if regex.firstMatch(in: name, range: NSRange(location: 0, length: name.utf16.count)) == nil {
            showError("VM Name must be 1-48 letters, numbers, underscores, or hyphens.")
            return
        }

        guard let selectedOpt = osPopUp.selectedItem?.representedObject as? OSSelectionOption else {
            showError("Please select a valid operating system.")
            return
        }

        var osVersion = selectedOpt.osVersion
        var customIso: String? = nil

        if selectedOpt.isCustomFilePicker {
            osVersion = "custom"
            customIso = isoPathField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            if customIso?.isEmpty ?? true || !FileManager.default.fileExists(atPath: customIso!) {
                showError("Please select a valid custom ISO file.")
                return
            }
        } else if let cachedPath = selectedOpt.customIsoPath, selectedOpt.osVersion == "custom" {
            osVersion = "custom"
            customIso = cachedPath
        }

        let cpus = [1, 2, 4, 8][cpuPopUp.indexOfSelectedItem]
        let rams = [1024, 2048, 4096, 8192, 16384][ramPopUp.indexOfSelectedItem]
        let diskGB = Int(diskField.stringValue) ?? 20
        let password = passwordField.stringValue.isEmpty ? "root" : passwordField.stringValue
        let desktopValue = desktopPopUp.selectedItem?.representedObject as? String
        let desktop = desktopValue?.isEmpty == false ? desktopValue : nil
        if desktop != nil && unattendedCheckbox.state != .on {
            showError("Desktop installation requires automated unattended installation.")
            return
        }
        let netMode = (netPopUp.selectedItem?.representedObject as? String) ?? (VMManager.shared.hasNetworkingEntitlement() ? "vmnet" : "vmnet-proxy")
        if desktop != nil && netMode == "none" {
            showError("Desktop installation requires networking to download FreeBSD packages.")
            return
        }
        let sshPort = Int(sshPortField.stringValue)
        let vncPort = Int(vncPortField.stringValue)

        let destDir = customStorageDir.map { "\($0)/\(name)" }
        let sharePath = sharePathField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let shareTag = shareTagField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let shareReadOnly = (shareReadOnlyCheckbox.state == .on)

        let options = CreateVMOptions(
            name: name,
            osVersion: osVersion,
            customIsoPath: customIso,
            isUnattended: (unattendedCheckbox.state == .on),
            desktop: desktop,
            cpus: cpus,
            ramMB: rams,
            diskSizeGB: diskGB,
            password: password,
            netMode: netMode,
            sshPort: sshPort,
            vncPort: vncPort,
            destinationDir: destDir,
            sharePath: sharePath.isEmpty ? nil : sharePath,
            shareTag: shareTag.isEmpty ? nil : shareTag,
            shareReadOnly: shareReadOnly,
            memPrealloc: (memPreallocCheckbox.state == .on)
        )

        isBusy = true
        setFormEnabled(false)
        revealProvisioningLogs()
        copyLogsButton.isHidden = false
        progressIndicator.isHidden = false
        progressIndicator.startAnimation(nil)
        statusLabel.stringValue = "Creating VM and preparing disk..."
        logTextView.string = "[Cube] Starting creation for '\(name)'...\n"

        VMManager.shared.createVM(options: options, progressHandler: { [weak self] chunk in
            guard let self = self else { return }
            self.logTextView.textStorage?.append(NSAttributedString(string: chunk, attributes: [
                .font: NSFont.userFixedPitchFont(ofSize: 10) ?? NSFont.systemFont(ofSize: 10)
            ]))
            self.logTextView.scrollToEndOfDocument(nil)
            if chunk.contains("Downloading") {
                self.statusLabel.stringValue = "Downloading ISO image..."
            } else if chunk.contains("desktop") || chunk.contains("Installing XFCE") || chunk.contains("Installing Plasma") {
                self.statusLabel.stringValue = "Installing desktop environment..."
            } else if chunk.contains("unattended") || chunk.contains("install") {
                self.statusLabel.stringValue = "Running installation..."
            }
        }) { [weak self] success, error in
            guard let self = self else { return }
            self.isBusy = false
            self.progressIndicator.stopAnimation(nil)
            self.setFormEnabled(true)

            if success {
                self.statusLabel.stringValue = "Created successfully!"
                DashboardWindowController.shared.selectVM(name: name)
                let alert = NSAlert()
                alert.messageText = "Virtual Machine Ready"
                let desktopNote = desktop == nil ? "" : " The desktop user is 'cube' and uses the root password you entered."
                alert.informativeText = "VM '\(name)' was created successfully.\(desktopNote) Would you like to start it now?"
                if desktop != nil {
                    alert.addButton(withTitle: "Start Native Display")
                    alert.addButton(withTitle: "Start VNC")
                    alert.addButton(withTitle: "Done")
                } else {
                    alert.addButton(withTitle: "Start VM")
                    alert.addButton(withTitle: "Done")
                }
                let resp = alert.runModal()
                if resp == .alertFirstButtonReturn || (desktop != nil && resp == .alertSecondButtonReturn) {
                    let mode: CubeCommandDisplayMode? = desktop == nil ? nil :
                        (resp == .alertFirstButtonReturn ? .native : .vnc)
                    VMManager.shared.startVM(name: name, displayMode: mode) { startSuccess, startError in
                        if !startSuccess {
                            DispatchQueue.main.async {
                                let errAlert = NSAlert()
                                errAlert.messageText = "Failed to Start Virtual Machine"
                                errAlert.informativeText = startError ?? "An unknown error occurred while starting the VM."
                                errAlert.alertStyle = .warning
                                errAlert.runModal()
                            }
                        } else if mode == .vnc {
                            VMManager.shared.refreshVMs { vms in
                                if let running = vms.first(where: { $0.name == name }) {
                                    VMManager.shared.openVNC(for: running)
                                }
                            }
                        }
                    }
                }
                self.window?.close()
            } else {
                self.statusLabel.stringValue = "Creation failed"
                self.showError(error ?? "An error occurred during VM creation.")
            }
        }
    }

    private func setFormEnabled(_ enabled: Bool) {
        nameField.isEnabled = enabled
        osPopUp.isEnabled = enabled
        unattendedCheckbox.isEnabled = enabled
        desktopPopUp.isEnabled = enabled && unattendedCheckbox.state == .on
        cpuPopUp.isEnabled = enabled
        ramPopUp.isEnabled = enabled
        memPreallocCheckbox?.isEnabled = enabled
        diskField.isEnabled = enabled
        securePasswordField.isEnabled = enabled
        visiblePasswordField.isEnabled = enabled
        passwordVisibilityButton.isEnabled = enabled
        netPopUp.isEnabled = enabled
        sshPortField.isEnabled = enabled
        vncPortField.isEnabled = enabled
        customizeStorageButton.isEnabled = enabled
        shareBrowseButton?.isEnabled = enabled
        shareTagField?.isEnabled = enabled
        shareReadOnlyCheckbox?.isEnabled = enabled
        createButton.isEnabled = enabled
        if enabled {
            osSelectionChanged()
        } else {
            isoPathField.isEnabled = false
            browseButton.isEnabled = false
        }
    }

    @objc private func chooseSharedFolderClicked() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Choose"
        panel.message = "Select a host directory to share with the guest VM"
        if panel.runModal() == .OK, let url = panel.url {
            sharePathField.stringValue = url.path
            let folderName = url.lastPathComponent.lowercased()
                .replacingOccurrences(of: " ", with: "-")
            if shareTagField.stringValue == "shared" || shareTagField.stringValue.isEmpty {
                shareTagField.stringValue = folderName
            }
        }
    }

    @objc private func clearSharedFolderClicked() {
        sharePathField.stringValue = ""
        shareTagField.stringValue = "shared"
        shareReadOnlyCheckbox.state = .off
    }

    private func showError(_ text: String) {
        let alert = NSAlert()
        alert.messageText = "Cannot Create Virtual Machine"
        alert.informativeText = text
        alert.alertStyle = .warning
        alert.runModal()
    }

    @objc private func copyLogsClicked() {
        let text = logTextView.string
        guard !text.isEmpty else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
        let orig = statusLabel.stringValue
        statusLabel.stringValue = "Logs copied to clipboard!"
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
            if self?.statusLabel.stringValue == "Logs copied to clipboard!" {
                self?.statusLabel.stringValue = orig
            }
        }
    }

    @objc private func nameFieldChanged() {
        updateStoragePathDisplay()
    }

    private func updateStoragePathDisplay() {
        let name = nameField?.stringValue.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let vmName = name.isEmpty ? "<name>" : name
        if let custom = customStorageDir {
            let short = custom.replacingOccurrences(of: NSHomeDirectory(), with: "~")
            storagePathLabel?.stringValue = "\(short)/\(vmName)"
            storagePathLabel?.textColor = CubeTokens.primaryText
            customizeStorageButton?.title = "Reset"
        } else {
            let base = VMManager.shared.vmsDirectory.replacingOccurrences(of: NSHomeDirectory(), with: "~")
            storagePathLabel?.stringValue = "\(base)/\(vmName)"
            storagePathLabel?.textColor = CubeTokens.secondaryText
            customizeStorageButton?.title = "Customize..."
        }
    }

    @objc private func customizeStorageClicked() {
        if customStorageDir != nil {
            customStorageDir = nil
            updateStoragePathDisplay()
            return
        }

        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Select"
        panel.message = "Choose Storage Location for VM"
        let currentDefault = VMManager.shared.vmsDirectory
        if FileManager.default.fileExists(atPath: currentDefault) {
            panel.directoryURL = URL(fileURLWithPath: currentDefault)
        }
        if panel.runModal() == .OK, let url = panel.url {
            customStorageDir = url.path
            updateStoragePathDisplay()
        }
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

        statusBarView.layer?.backgroundColor = CubeTokens.statusBarBackground.cgColor

        refreshCardLayers(in: window.contentView)
        window.contentView?.needsDisplay = true
    }

    private func refreshCardLayers(in view: NSView?) {
        guard let view = view else { return }
        if let card = view as? NewVMCardView {
            card.updateColors()
        }
        for sub in view.subviews {
            refreshCardLayers(in: sub)
        }
    }
}
