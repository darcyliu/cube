import Foundation
import AppKit

class VMLogWindowController: NSWindowController, NSWindowDelegate, NSSearchFieldDelegate {
    static let shared = VMLogWindowController()

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

    convenience init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 840, height: 560),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Cube VM Logs"
        window.minSize = NSSize(width: 680, height: 400)
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
    }

    // MARK: - UI Layout

    private func setupUI() {
        guard let contentView = window?.contentView else { return }

        // 1. Header Toolbar View (Height: 74)
        let headerView = NSView(frame: NSRect(x: 0, y: contentView.bounds.height - 74, width: contentView.bounds.width, height: 74))
        headerView.autoresizingMask = [.width, .minYMargin]
        headerView.wantsLayer = true
        headerView.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor

        let row1Y: CGFloat = 40
        let row2Y: CGFloat = 8

        // Row 1: VM Selector Label & PopUp
        let vmLabel = NSTextField(labelWithString: "Virtual Machine:")
        vmLabel.font = NSFont.systemFont(ofSize: 11, weight: .medium)
        vmLabel.textColor = .secondaryLabelColor
        vmLabel.frame = NSRect(x: 16, y: row1Y + 2, width: 100, height: 18)
        headerView.addSubview(vmLabel)

        vmPicker = NSPopUpButton(frame: NSRect(x: 120, y: row1Y - 2, width: 180, height: 24), pullsDown: false)
        vmPicker.target = self
        vmPicker.action = #selector(vmPickerChanged)
        vmPicker.font = NSFont.systemFont(ofSize: 12)
        headerView.addSubview(vmPicker)

        // Log Type Segmented Control
        logTypeSegment = NSSegmentedControl(labels: ["Console", "VMM", "Manager"], trackingMode: .selectOne, target: self, action: #selector(logTypeChanged))
        logTypeSegment.selectedSegment = 0
        logTypeSegment.segmentStyle = .texturedRounded
        logTypeSegment.font = NSFont.systemFont(ofSize: 11)
        logTypeSegment.frame = NSRect(x: 312, y: row1Y - 1, width: 200, height: 24)
        headerView.addSubview(logTypeSegment)

        // Action Buttons (Copy, Finder, Clear)
        let finderBtn = NSButton(title: "Finder", target: self, action: #selector(revealInFinderClicked))
        finderBtn.bezelStyle = .rounded
        finderBtn.controlSize = .small
        finderBtn.font = NSFont.systemFont(ofSize: 11)
        finderBtn.frame = NSRect(x: headerView.bounds.width - 76, y: row1Y - 1, width: 64, height: 22)
        finderBtn.autoresizingMask = [.minXMargin]
        headerView.addSubview(finderBtn)

        let clearBtn = NSButton(title: "Clear View", target: self, action: #selector(clearViewClicked))
        clearBtn.bezelStyle = .rounded
        clearBtn.controlSize = .small
        clearBtn.font = NSFont.systemFont(ofSize: 11)
        clearBtn.frame = NSRect(x: headerView.bounds.width - 162, y: row1Y - 1, width: 82, height: 22)
        clearBtn.autoresizingMask = [.minXMargin]
        headerView.addSubview(clearBtn)

        let copyBtn = NSButton(title: "Copy", target: self, action: #selector(copyClicked(_:)))
        copyBtn.bezelStyle = .rounded
        copyBtn.controlSize = .small
        copyBtn.font = NSFont.systemFont(ofSize: 11)
        copyBtn.frame = NSRect(x: headerView.bounds.width - 228, y: row1Y - 1, width: 62, height: 22)
        copyBtn.autoresizingMask = [.minXMargin]
        headerView.addSubview(copyBtn)

        // Row 2: Search Bar, Auto-scroll & Wrap Toggles
        searchField = NSSearchField(frame: NSRect(x: 16, y: row2Y, width: 284, height: 22))
        searchField.placeholderString = "Filter log output..."
        searchField.delegate = self
        searchField.font = NSFont.systemFont(ofSize: 11)
        headerView.addSubview(searchField)

        autoScrollCheckbox = NSButton(checkboxWithTitle: "Auto-scroll to Bottom", target: self, action: #selector(autoScrollToggled))
        autoScrollCheckbox.state = .on
        autoScrollCheckbox.font = NSFont.systemFont(ofSize: 11)
        autoScrollCheckbox.frame = NSRect(x: 312, y: row2Y + 2, width: 160, height: 18)
        headerView.addSubview(autoScrollCheckbox)

        wrapLinesCheckbox = NSButton(checkboxWithTitle: "Wrap Lines", target: self, action: #selector(wrapLinesToggled))
        wrapLinesCheckbox.state = .on
        wrapLinesCheckbox.font = NSFont.systemFont(ofSize: 11)
        wrapLinesCheckbox.frame = NSRect(x: 480, y: row2Y + 2, width: 100, height: 18)
        headerView.addSubview(wrapLinesCheckbox)

        statusLabel = NSTextField(labelWithString: "")
        statusLabel.font = NSFont.systemFont(ofSize: 10)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.alignment = .right
        statusLabel.frame = NSRect(x: headerView.bounds.width - 230, y: row2Y + 2, width: 216, height: 16)
        statusLabel.autoresizingMask = [.minXMargin]
        headerView.addSubview(statusLabel)

        // Hairline separator below header
        let sep = NSBox(frame: NSRect(x: 0, y: 0, width: headerView.bounds.width, height: 1))
        sep.boxType = .separator
        sep.autoresizingMask = [.width]
        headerView.addSubview(sep)

        contentView.addSubview(headerView)

        // 2. Log Text View (Dark Terminal Style)
        let logContainerHeight = contentView.bounds.height - 74
        scrollView = NSScrollView(frame: NSRect(x: 0, y: 0, width: contentView.bounds.width, height: logContainerHeight))
        scrollView.autoresizingMask = [.width, .height]
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder

        let contentSize = scrollView.contentSize
        textView = NSTextView(frame: NSRect(origin: .zero, size: contentSize))
        textView.minSize = NSSize(width: 0.0, height: contentSize.height)
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = true
        textView.autoresizingMask = [.width]
        textView.textContainer?.containerSize = NSSize(width: contentSize.width, height: CGFloat.greatestFiniteMagnitude)
        textView.textContainer?.widthTracksTextView = true

        // Terminal Aesthetic
        textView.backgroundColor = NSColor(calibratedRed: 0.10, green: 0.10, blue: 0.12, alpha: 1.0)
        textView.textColor = NSColor(calibratedRed: 0.90, green: 0.92, blue: 0.94, alpha: 1.0)
        textView.font = NSFont.monospacedSystemFont(ofSize: 11.5, weight: .regular)
        textView.isEditable = false
        textView.isSelectable = true
        textView.textContainerInset = NSSize(width: 12, height: 10)

        scrollView.documentView = textView
        contentView.addSubview(scrollView)
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
            rawLogContent = content
            applyFilterAndDisplay(resetPosition: resetPosition)
            let kb = Double(fileSize) / 1024.0
            statusLabel.stringValue = String(format: "%.1f KB • %@", kb, modDate.formatted(date: .omitted, time: .standard))
        }
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
        textView.textContainer?.widthTracksTextView = shouldWrap
        if shouldWrap {
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
            btn.title = "Copied!"
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                btn.title = "Copy"
            }
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
}
