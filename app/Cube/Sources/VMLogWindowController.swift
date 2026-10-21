import Foundation
import AppKit

class VMLogWindowController: NSWindowController, NSWindowDelegate, NSSearchFieldDelegate {
    static let shared = VMLogWindowController()

    private var rootVisualEffect: NSVisualEffectView!
    private var headerView: NSView!
    private var vmPicker: NSPopUpButton!
    private var logTypeSegment: NSSegmentedControl!
    private var searchField: NSSearchField!
    private var autoScrollCheckbox: NSButton!
    private var wrapLinesCheckbox: NSButton!
    private var textView: NSTextView!
    private var scrollView: NSScrollView!
    private var statusLabel: NSTextField!

    private var currentVMName: String?
    private var refreshTimer: Timer?
    private var rawLogContent: String = ""
    private var lastFileModificationDate: Date?

    enum LogType: Int {
        case console = 0
        case vmm = 1
        case manager = 2

        var filename: String {
            switch self {
            case .console: return "console.log"
            case .vmm: return "vmm.log"
            case .manager: return "manager.log"
            }
        }

        var displayName: String {
            switch self {
            case .console: return "Serial Console (console.log)"
            case .vmm: return "Hypervisor (vmm.log)"
            case .manager: return "Manager Daemon (manager.log)"
            }
        }
    }

    deinit {
        NSWorkspace.shared.notificationCenter.removeObserver(self)
    }

    convenience init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 880, height: 580),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Cube VM Logs"
        window.minSize = NSSize(width: 700, height: 420)
        window.center()
        window.isReleasedWhenClosed = false

        self.init(window: window)
        window.delegate = self
        setupUI()
    }

    static func show(for vmName: String? = nil) {
        AppDelegate.ensureRegularActivation()
        shared.showWindow(nil)
        if let name = vmName {
            shared.selectVM(name: name)
        } else if shared.currentVMName == nil, let first = VMManager.shared.vms.first?.name {
            shared.selectVM(name: first)
        }
        NSApp.activate(ignoringOtherApps: true)
    }

    override func showWindow(_ sender: Any?) {
        super.showWindow(sender)
        window?.makeKeyAndOrderFront(sender)
        updateVMPicker()
        loadLogs()
        startTimer()
    }

    func windowWillClose(_ notification: Notification) {
        stopTimer()
        let hasOtherWindows = PreferencesWindowController.shared.window?.isVisible == true ||
                              DashboardWindowController.shared.window?.isVisible == true ||
                              (NewVMWindowController.shared?.window?.isVisible == true)
        if !hasOtherWindows {
            NSApp.setActivationPolicy(.accessory)
        }
    }

    // MARK: - UI Construction (Auto Layout & Frosted Glass)

    private func setupUI() {
        guard let window = self.window else { return }

        // Root Liquid Translucent Glass (underWindowBackground blur behind window)
        rootVisualEffect = NSVisualEffectView(frame: window.contentView?.bounds ?? NSRect(x: 0, y: 0, width: 880, height: 580))
        rootVisualEffect.material = .underWindowBackground
        rootVisualEffect.blendingMode = .behindWindow
        rootVisualEffect.state = .active
        rootVisualEffect.autoresizingMask = [.width, .height]
        rootVisualEffect.wantsLayer = true
        window.contentView = rootVisualEffect

        // 1. Header Toolbar View (Height: 78pt, traffic light clearance)
        headerView = NSView()
        headerView.translatesAutoresizingMaskIntoConstraints = false
        headerView.wantsLayer = true
        headerView.layer?.backgroundColor = CubeTokens.statusBarBackground.cgColor
        rootVisualEffect.addSubview(headerView)

        // Row 1: VM Selector Label & PopUp
        let vmLabel = NSTextField(labelWithString: "Virtual Machine:")
        vmLabel.translatesAutoresizingMaskIntoConstraints = false
        vmLabel.font = NSFont.systemFont(ofSize: 11.5, weight: .medium)
        vmLabel.textColor = CubeTokens.secondaryText
        headerView.addSubview(vmLabel)

        vmPicker = NSPopUpButton(frame: .zero, pullsDown: false)
        vmPicker.translatesAutoresizingMaskIntoConstraints = false
        vmPicker.target = self
        vmPicker.action = #selector(vmPickerChanged)
        vmPicker.font = NSFont.systemFont(ofSize: 12)
        headerView.addSubview(vmPicker)

        // Log Type Segmented Control
        logTypeSegment = NSSegmentedControl(labels: ["Console", "VMM", "Manager"], trackingMode: .selectOne, target: self, action: #selector(logTypeChanged))
        logTypeSegment.translatesAutoresizingMaskIntoConstraints = false
        logTypeSegment.selectedSegment = 0
        logTypeSegment.segmentStyle = .texturedRounded
        logTypeSegment.font = NSFont.systemFont(ofSize: 11)
        headerView.addSubview(logTypeSegment)

        // Action Buttons (Copy, Finder, Clear)
        let finderBtn = NSButton(title: "Finder", target: self, action: #selector(revealInFinderClicked))
        finderBtn.translatesAutoresizingMaskIntoConstraints = false
        finderBtn.bezelStyle = .rounded
        finderBtn.controlSize = .small
        finderBtn.font = NSFont.systemFont(ofSize: 11)
        headerView.addSubview(finderBtn)

        let clearBtn = NSButton(title: "Clear View", target: self, action: #selector(clearViewClicked))
        clearBtn.translatesAutoresizingMaskIntoConstraints = false
        clearBtn.bezelStyle = .rounded
        clearBtn.controlSize = .small
        clearBtn.font = NSFont.systemFont(ofSize: 11)
        headerView.addSubview(clearBtn)

        let copyBtn = NSButton(title: "Copy", target: self, action: #selector(copyClicked(_:)))
        copyBtn.translatesAutoresizingMaskIntoConstraints = false
        copyBtn.bezelStyle = .rounded
        copyBtn.controlSize = .small
        copyBtn.font = NSFont.systemFont(ofSize: 11)
        headerView.addSubview(copyBtn)

        // Row 2: Search Bar, Auto-scroll & Wrap Toggles, Status Label
        searchField = NSSearchField()
        searchField.translatesAutoresizingMaskIntoConstraints = false
        searchField.placeholderString = "Filter log output..."
        searchField.delegate = self
        searchField.font = NSFont.systemFont(ofSize: 11)
        headerView.addSubview(searchField)

        autoScrollCheckbox = NSButton(checkboxWithTitle: "Auto-scroll to Bottom", target: self, action: #selector(autoScrollToggled))
        autoScrollCheckbox.translatesAutoresizingMaskIntoConstraints = false
        autoScrollCheckbox.state = .on
        autoScrollCheckbox.font = NSFont.systemFont(ofSize: 11)
        headerView.addSubview(autoScrollCheckbox)

        wrapLinesCheckbox = NSButton(checkboxWithTitle: "Wrap Lines", target: self, action: #selector(wrapLinesToggled))
        wrapLinesCheckbox.translatesAutoresizingMaskIntoConstraints = false
        wrapLinesCheckbox.state = .on
        wrapLinesCheckbox.font = NSFont.systemFont(ofSize: 11)
        headerView.addSubview(wrapLinesCheckbox)

        statusLabel = NSTextField(labelWithString: "")
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        statusLabel.font = NSFont.monospacedSystemFont(ofSize: 10, weight: .regular)
        statusLabel.textColor = CubeTokens.secondaryText
        statusLabel.alignment = .right
        headerView.addSubview(statusLabel)

        // Hairline separator below header
        let sep = NSBox()
        sep.translatesAutoresizingMaskIntoConstraints = false
        sep.boxType = .custom
        sep.borderWidth = 0
        sep.fillColor = CubeTokens.separatorColor
        headerView.addSubview(sep)

        // 2. Log Text View (Terminal Dark Obsidian Style)
        scrollView = NSScrollView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        rootVisualEffect.addSubview(scrollView)

        textView = NSTextView()
        textView.minSize = NSSize(width: 0.0, height: 0.0)
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.textContainer?.widthTracksTextView = true

        // Terminal Aesthetic
        textView.backgroundColor = NSColor(calibratedRed: 0.08, green: 0.09, blue: 0.11, alpha: 0.88)
        textView.textColor = NSColor(calibratedRed: 0.90, green: 0.93, blue: 0.96, alpha: 1.0)
        textView.font = NSFont.monospacedSystemFont(ofSize: 11.5, weight: .regular)
        textView.isEditable = false
        textView.isSelectable = true
        textView.textContainerInset = NSSize(width: 14, height: 12)

        scrollView.documentView = textView

        // Auto Layout Constraints
        NSLayoutConstraint.activate([
            // Header view pinned to top
            headerView.topAnchor.constraint(equalTo: rootVisualEffect.topAnchor),
            headerView.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            headerView.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            headerView.heightAnchor.constraint(equalToConstant: 78),

            // Row 1 constraints (Y = 16 from top of headerView, clearing window buttons)
            vmLabel.leadingAnchor.constraint(equalTo: headerView.leadingAnchor, constant: 18),
            vmLabel.topAnchor.constraint(equalTo: headerView.topAnchor, constant: 14),
            vmLabel.centerYAnchor.constraint(equalTo: vmPicker.centerYAnchor),

            vmPicker.leadingAnchor.constraint(equalTo: vmLabel.trailingAnchor, constant: 8),
            vmPicker.widthAnchor.constraint(equalToConstant: 170),
            vmPicker.heightAnchor.constraint(equalToConstant: 24),

            logTypeSegment.leadingAnchor.constraint(equalTo: vmPicker.trailingAnchor, constant: 12),
            logTypeSegment.centerYAnchor.constraint(equalTo: vmPicker.centerYAnchor),
            logTypeSegment.widthAnchor.constraint(equalToConstant: 200),
            logTypeSegment.heightAnchor.constraint(equalToConstant: 22),

            finderBtn.trailingAnchor.constraint(equalTo: headerView.trailingAnchor, constant: -18),
            finderBtn.centerYAnchor.constraint(equalTo: vmPicker.centerYAnchor),
            finderBtn.widthAnchor.constraint(equalToConstant: 64),

            clearBtn.trailingAnchor.constraint(equalTo: finderBtn.leadingAnchor, constant: -6),
            clearBtn.centerYAnchor.constraint(equalTo: vmPicker.centerYAnchor),
            clearBtn.widthAnchor.constraint(equalToConstant: 82),

            copyBtn.trailingAnchor.constraint(equalTo: clearBtn.leadingAnchor, constant: -6),
            copyBtn.centerYAnchor.constraint(equalTo: vmPicker.centerYAnchor),
            copyBtn.widthAnchor.constraint(equalToConstant: 62),
            copyBtn.leadingAnchor.constraint(greaterThanOrEqualTo: logTypeSegment.trailingAnchor, constant: 10),

            // Row 2 constraints (Y = 46 from top of headerView)
            searchField.leadingAnchor.constraint(equalTo: headerView.leadingAnchor, constant: 18),
            searchField.topAnchor.constraint(equalTo: headerView.topAnchor, constant: 46),
            searchField.widthAnchor.constraint(equalToConstant: 240),
            searchField.heightAnchor.constraint(equalToConstant: 22),

            autoScrollCheckbox.leadingAnchor.constraint(equalTo: searchField.trailingAnchor, constant: 14),
            autoScrollCheckbox.centerYAnchor.constraint(equalTo: searchField.centerYAnchor),

            wrapLinesCheckbox.leadingAnchor.constraint(equalTo: autoScrollCheckbox.trailingAnchor, constant: 12),
            wrapLinesCheckbox.centerYAnchor.constraint(equalTo: searchField.centerYAnchor),

            statusLabel.trailingAnchor.constraint(equalTo: headerView.trailingAnchor, constant: -18),
            statusLabel.centerYAnchor.constraint(equalTo: searchField.centerYAnchor),
            statusLabel.leadingAnchor.constraint(greaterThanOrEqualTo: wrapLinesCheckbox.trailingAnchor, constant: 10),

            // Separator pinned to bottom of header
            sep.leadingAnchor.constraint(equalTo: headerView.leadingAnchor),
            sep.trailingAnchor.constraint(equalTo: headerView.trailingAnchor),
            sep.bottomAnchor.constraint(equalTo: headerView.bottomAnchor),
            sep.heightAnchor.constraint(equalToConstant: 1),

            // Scroll View pinned below header to bottom of root
            scrollView.topAnchor.constraint(equalTo: headerView.bottomAnchor),
            scrollView.leadingAnchor.constraint(equalTo: rootVisualEffect.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: rootVisualEffect.trailingAnchor),
            scrollView.bottomAnchor.constraint(equalTo: rootVisualEffect.bottomAnchor)
        ])

        // Auto Layout must size the viewport before initializing the document width.
        rootVisualEffect.layoutSubtreeIfNeeded()
        wrapLinesToggled()

        // Accessibility Listeners
        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(accessibilityDisplayOptionsDidChange),
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification,
            object: nil
        )
        updateAccessibilityPreferences()
    }

    // MARK: - VM Selection & Log Loading

    func selectVM(name: String) {
        currentVMName = name
        updateVMPicker()
        loadLogs(resetPosition: true)
    }

    private func updateVMPicker() {
        vmPicker.removeAllItems()
        let vms = VMManager.shared.vms
        if vms.isEmpty {
            vmPicker.addItem(withTitle: "(No VMs)")
            vmPicker.isEnabled = false
            return
        }
        vmPicker.isEnabled = true
        for vm in vms {
            vmPicker.addItem(withTitle: vm.name)
        }
        if let current = currentVMName, vmPicker.itemTitles.contains(current) {
            vmPicker.selectItem(withTitle: current)
        } else if let first = vms.first {
            currentVMName = first.name
            vmPicker.selectItem(withTitle: first.name)
        }
        window?.title = "Cube VM Logs - \(currentVMName ?? "")"
    }

    private var currentLogType: LogType {
        return LogType(rawValue: logTypeSegment.selectedSegment) ?? .console
    }

    private var currentLogPath: String? {
        guard let name = currentVMName, !name.isEmpty else { return nil }
        let home = NSHomeDirectory()
        return "\(home)/.local/state/cube/\(name)/\(currentLogType.filename)"
    }

    private func loadLogs(silent: Bool = false, resetPosition: Bool = false) {
        guard let path = currentLogPath else {
            setDisplayedText("No Virtual Machine selected.")
            statusLabel.stringValue = ""
            return
        }

        let fileManager = FileManager.default
        if !fileManager.fileExists(atPath: path) {
            rawLogContent = ""
            setDisplayedText("Log file does not exist yet:\n\(path)\n\nLogs will appear once '\(currentVMName ?? "")' starts.")
            statusLabel.stringValue = "Offline / Not started"
            return
        }

        guard let attrs = try? fileManager.attributesOfItem(atPath: path),
              let modDate = attrs[.modificationDate] as? Date,
              let fileSize = attrs[.size] as? UInt64 else {
            return
        }

        if silent && lastFileModificationDate == modDate {
            return
        }
        lastFileModificationDate = modDate

        if let data = try? Data(contentsOf: URL(fileURLWithPath: path)),
           let content = String(data: data, encoding: .utf8) ?? String(data: data, encoding: .ascii) {
            rawLogContent = VMLogWindowController.cleanAnsiCodes(content)
            applyFilterAndDisplay(resetPosition: resetPosition)
            let kb = Double(fileSize) / 1024.0
            statusLabel.stringValue = String(format: "%.1f KB • %@", kb, modDate.formatted(date: .omitted, time: .standard))
        }
    }

    private static let ansiRegex: NSRegularExpression? = {
        let pattern = #"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~]|\?[0-9a-zA-Z])|\[[0-9]{1,3}(?:;[0-9]{1,3})*[mH]|\[[0-2]?[JK]"#
        return try? NSRegularExpression(pattern: pattern, options: [])
    }()

    static func cleanAnsiCodes(_ input: String) -> String {
        guard let regex = ansiRegex, !input.isEmpty else { return input }
        let range = NSRange(input.startIndex..<input.endIndex, in: input)
        var cleaned = regex.stringByReplacingMatches(in: input, options: [], range: range, withTemplate: "")
        cleaned = cleaned.replacingOccurrences(of: "\u{001B}", with: "")
        cleaned = cleaned.replacingOccurrences(of: "\u{0008}", with: "")
        return cleaned
    }

    private func applyFilterAndDisplay(resetPosition: Bool = false) {
        let filter = searchField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let textToDisplay: String
        if filter.isEmpty {
            textToDisplay = rawLogContent.isEmpty ? "(Log is empty)" : rawLogContent
        } else {
            let lines = rawLogContent.components(separatedBy: "\n")
            let matched = lines.filter { $0.localizedCaseInsensitiveContains(filter) }
            textToDisplay = matched.isEmpty ? "(No lines matching '\(filter)')" : matched.joined(separator: "\n")
        }

        setDisplayedText(textToDisplay)

        if autoScrollCheckbox.state == .on || resetPosition {
            DispatchQueue.main.async {
                self.textView.scrollToEndOfDocument(nil)
            }
        }
    }

    private func setDisplayedText(_ text: String) {
        textView.string = text
    }

    // MARK: - Timer & Live Follow

    private func startTimer() {
        refreshTimer?.invalidate()
        refreshTimer = Timer.scheduledTimer(withTimeInterval: 1.5, repeats: true) { [weak self] _ in
            self?.loadLogs(silent: true)
        }
    }

    private func stopTimer() {
        refreshTimer?.invalidate()
        refreshTimer = nil
    }

    // MARK: - Control Actions

    @objc private func vmPickerChanged() {
        if let selected = vmPicker.selectedItem?.title {
            selectVM(name: selected)
        }
    }

    @objc private func logTypeChanged() {
        lastFileModificationDate = nil
        loadLogs(resetPosition: true)
    }

    func controlTextDidChange(_ obj: Notification) {
        applyFilterAndDisplay()
    }

    @objc private func autoScrollToggled() {
        if autoScrollCheckbox.state == .on {
            textView.scrollToEndOfDocument(nil)
        }
    }

    @objc private func wrapLinesToggled() {
        let shouldWrap = (wrapLinesCheckbox.state == .on)
        textView.isHorizontallyResizable = !shouldWrap
        textView.autoresizingMask = shouldWrap ? [.width] : []
        textView.textContainer?.widthTracksTextView = shouldWrap
        if shouldWrap {
            textView.setFrameSize(NSSize(width: scrollView.contentSize.width, height: textView.frame.height))
            textView.textContainer?.containerSize = NSSize(width: scrollView.contentSize.width, height: CGFloat.greatestFiniteMagnitude)
        } else {
            textView.textContainer?.containerSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        }
    }

    @objc private func copyClicked(_ sender: Any?) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(textView.string, forType: .string)

        if let btn = sender as? NSButton {
            CubeTokens.animateCopyFeedback(button: btn)
        }
    }

    @objc private func clearViewClicked() {
        rawLogContent = ""
        setDisplayedText("(View cleared. New output will append when file updates)")
    }

    @objc private func revealInFinderClicked() {
        guard let name = currentVMName, !name.isEmpty else { return }
        let home = NSHomeDirectory()
        let dir = "\(home)/.local/state/cube/\(name)"
        if let logPath = currentLogPath, FileManager.default.fileExists(atPath: logPath) {
            NSWorkspace.shared.selectFile(logPath, inFileViewerRootedAtPath: dir)
        } else {
            NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: dir)
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

        headerView.layer?.backgroundColor = CubeTokens.statusBarBackground.cgColor
        window.contentView?.needsDisplay = true
    }
}
