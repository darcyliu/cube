import Foundation
import AppKit

private class FlippedSheetView: NSView {
    override var isFlipped: Bool { return true }
}

private class ActionButton: NSButton {
    var onClick: ((ActionButton) -> Void)?

    init(title: String) {
        super.init(frame: .zero)
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

class VMCommandsSheetController: NSWindowController, NSWindowDelegate {
    static var current: VMCommandsSheetController?

    private let vm: VMInfo
    private weak var parentSheetWindow: NSWindow?

    init(vm: VMInfo, parent: NSWindow?) {
        self.vm = vm
        self.parentSheetWindow = parent

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 600, height: 570),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.title = "\(vm.name) — CLI Commands & Launch Info"
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.delegate = self

        setupUI()
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    static func show(for vm: VMInfo, on parentWindow: NSWindow? = nil) {
        let parent = parentWindow ?? DashboardWindowController.shared.window
        let controller = VMCommandsSheetController(vm: vm, parent: parent)
        current = controller

        if let parent = parent, parent.isVisible {
            parent.beginSheet(controller.window!) { _ in
                if current === controller {
                    current = nil
                }
            }
        } else {
            controller.showWindow(nil)
            controller.window?.center()
            controller.window?.makeKeyAndOrderFront(nil)
        }
    }

    func windowWillClose(_ notification: Notification) {
        if VMCommandsSheetController.current === self {
            VMCommandsSheetController.current = nil
        }
    }

    private func setupUI() {
        guard let window = window else { return }

        let contentView = FlippedSheetView(frame: window.contentView!.bounds)
        contentView.autoresizingMask = [.width, .height]
        window.contentView = contentView

        let margin: CGFloat = 24
        let contentWidth = contentView.bounds.width - (margin * 2)
        var y: CGFloat = 22

        // 1. Header Bar: Icon, Name, Subtitle, Status Badge
        let iconView = NSImageView(frame: NSRect(x: margin, y: y + 2, width: 34, height: 34))
        let cfg = NSImage.SymbolConfiguration(pointSize: 24, weight: .regular)
        if let img = NSImage(systemSymbolName: "terminal.fill", accessibilityDescription: nil)?.withSymbolConfiguration(cfg) {
            iconView.image = img
            iconView.contentTintColor = CubeTokens.accent
        }
        contentView.addSubview(iconView)

        let titleLabel = NSTextField(labelWithString: vm.name)
        titleLabel.font = NSFont.systemFont(ofSize: 19, weight: .bold)
        titleLabel.textColor = CubeTokens.primaryText
        titleLabel.frame = NSRect(x: margin + 44, y: y, width: contentWidth - 44, height: 24)
        contentView.addSubview(titleLabel)

        let subtitleLabel = NSTextField(labelWithString: "CLI Execution & Automation Commands")
        subtitleLabel.font = NSFont.systemFont(ofSize: 11, weight: .regular)
        subtitleLabel.textColor = CubeTokens.secondaryText
        subtitleLabel.frame = NSRect(x: margin + 44, y: y + 24, width: contentWidth - 44, height: 16)
        contentView.addSubview(subtitleLabel)

        y += 54

        // 2. Card 1: cubectl Daemon Commands
        let card1Height: CGFloat = 138
        let card1 = createCard(frame: NSRect(x: margin, y: y, width: contentWidth, height: card1Height))
        contentView.addSubview(card1)

        let c1Title = NSTextField(labelWithString: "CUBECTL MANAGER COMMANDS")
        c1Title.font = CubeTokens.sectionHeaderFont
        c1Title.textColor = CubeTokens.secondaryText
        c1Title.frame = NSRect(x: 18, y: 14, width: contentWidth - 36, height: 16)
        card1.addSubview(c1Title)

        var cy: CGFloat = 34
        addCommandRow(label: "Start VM:", command: "cubectl start \(vm.name)", in: card1, y: cy, width: contentWidth)
        cy += 24
        addCommandRow(label: "Connect via SSH:", command: "cubectl ssh \(vm.name)", in: card1, y: cy, width: contentWidth)
        cy += 24
        addCommandRow(label: "Query Status:", command: "cubectl status \(vm.name)", in: card1, y: cy, width: contentWidth)
        cy += 24
        addCommandRow(label: "Stop VM:", command: "cubectl stop \(vm.name)", in: card1, y: cy, width: contentWidth)

        y += card1Height + 12

        // 3. Card 2: Direct Hypervisor Engine Command (cube)
        let card2Height: CGFloat = 176
        let card2 = createCard(frame: NSRect(x: margin, y: y, width: contentWidth, height: card2Height))
        contentView.addSubview(card2)

        let c2Title = NSTextField(labelWithString: "DIRECT HYPERVISOR ENGINE COMMAND (CUBE)")
        c2Title.font = CubeTokens.sectionHeaderFont
        c2Title.textColor = CubeTokens.secondaryText
        c2Title.frame = NSRect(x: 18, y: 14, width: contentWidth - 36, height: 16)
        card2.addSubview(c2Title)

        let c2Sub = NSTextField(labelWithString: "Run directly via Apple Silicon Hypervisor.framework without manager daemon:")
        c2Sub.font = NSFont.systemFont(ofSize: 11, weight: .regular)
        c2Sub.textColor = CubeTokens.tertiaryText
        c2Sub.frame = NSRect(x: 18, y: 31, width: contentWidth - 36, height: 15)
        card2.addSubview(c2Sub)

        let cubeCmd = vm.generateCubeCommand()
        let scrollView = NSScrollView(frame: NSRect(x: 18, y: 50, width: contentWidth - 36, height: 82))
        scrollView.wantsLayer = true
        scrollView.layer?.cornerRadius = 6
        scrollView.layer?.borderWidth = 1.0
        scrollView.layer?.borderColor = CubeTokens.cardBorder.cgColor
        scrollView.backgroundColor = NSColor(white: 0.08, alpha: 1.0)
        scrollView.drawsBackground = true
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true

        let cmdTextView = NSTextView(frame: NSRect(x: 0, y: 0, width: scrollView.contentSize.width, height: scrollView.contentSize.height))
        cmdTextView.string = cubeCmd
        cmdTextView.font = NSFont.monospacedSystemFont(ofSize: 10.5, weight: .regular)
        cmdTextView.textColor = NSColor(red: 0.90, green: 0.94, blue: 0.98, alpha: 1.0)
        cmdTextView.backgroundColor = NSColor(white: 0.08, alpha: 1.0)
        cmdTextView.drawsBackground = false
        cmdTextView.isEditable = false
        cmdTextView.isSelectable = true
        cmdTextView.isVerticallyResizable = true
        cmdTextView.isHorizontallyResizable = false
        cmdTextView.textContainer?.containerSize = NSSize(width: scrollView.contentSize.width, height: CGFloat.greatestFiniteMagnitude)
        cmdTextView.textContainer?.widthTracksTextView = true

        scrollView.documentView = cmdTextView
        card2.addSubview(scrollView)

        let copyEngineBtn = ActionButton(title: "Copy Direct Command")
        copyEngineBtn.bezelStyle = .rounded
        copyEngineBtn.controlSize = .small
        copyEngineBtn.font = NSFont.systemFont(ofSize: 11)
        copyEngineBtn.frame = NSRect(x: contentWidth - 18 - 150, y: 140, width: 150, height: 24)
        copyEngineBtn.onClick = { btn in
            let pb = NSPasteboard.general
            pb.clearContents()
            pb.setString(cubeCmd, forType: .string)
            btn.title = "Copied! ✓"
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { [weak btn] in
                btn?.title = "Copy Direct Command"
            }
        }
        card2.addSubview(copyEngineBtn)

        y += card2Height + 12

        // 4. Card 3: Storage Locations
        let card3Height: CGFloat = 86
        let card3 = createCard(frame: NSRect(x: margin, y: y, width: contentWidth, height: card3Height))
        contentView.addSubview(card3)

        let c3Title = NSTextField(labelWithString: "STORAGE & FILE LOCATIONS")
        c3Title.font = CubeTokens.sectionHeaderFont
        c3Title.textColor = CubeTokens.secondaryText
        c3Title.frame = NSRect(x: 18, y: 12, width: contentWidth - 36, height: 16)
        card3.addSubview(c3Title)

        let configPath = vm.config ?? "\(VMManager.shared.vmsDirectory)/\(vm.name)/\(vm.name).json"
        let statePath = "\(NSHomeDirectory())/.local/state/cube/\(vm.name)"

        addPathRow(label: "Config File:", path: configPath, in: card3, y: 32, width: contentWidth)
        addPathRow(label: "Runtime State:", path: statePath, in: card3, y: 56, width: contentWidth)

        y += card3Height + 16

        // 5. Done Button
        let doneButton = ActionButton(title: "Done")
        doneButton.bezelStyle = .rounded
        doneButton.keyEquivalent = "\r"
        doneButton.font = NSFont.systemFont(ofSize: 12, weight: .semibold)
        doneButton.frame = NSRect(x: contentView.bounds.width - margin - 82, y: y, width: 82, height: 32)
        doneButton.onClick = { [weak self] _ in
            self?.doneClicked()
        }
        contentView.addSubview(doneButton)
    }

    private func createCard(frame: NSRect) -> FlippedSheetView {
        let card = FlippedSheetView(frame: frame)
        card.wantsLayer = true
        card.layer?.cornerRadius = CubeTokens.cardCornerRadius
        card.layer?.borderWidth = CubeTokens.cardBorderWidth
        card.layer?.borderColor = CubeTokens.cardBorder.cgColor
        card.layer?.backgroundColor = CubeTokens.cardBackground.cgColor
        return card
    }

    private func addCommandRow(label: String, command: String, in container: NSView, y: CGFloat, width: CGFloat) {
        let lbl = NSTextField(labelWithString: label)
        lbl.font = CubeTokens.bodyLabelFont
        lbl.textColor = CubeTokens.secondaryText
        lbl.frame = NSRect(x: 18, y: y, width: 120, height: 18)
        container.addSubview(lbl)

        let cmdBox = NSTextField(labelWithString: command)
        cmdBox.font = CubeTokens.bodyValueFont
        cmdBox.textColor = CubeTokens.primaryText
        cmdBox.lineBreakMode = .byTruncatingTail
        cmdBox.frame = NSRect(x: 142, y: y, width: width - 142 - 80, height: 18)
        container.addSubview(cmdBox)

        let copyBtn = ActionButton(title: "Copy")
        copyBtn.bezelStyle = .rounded
        copyBtn.controlSize = .small
        copyBtn.font = NSFont.systemFont(ofSize: 11)
        copyBtn.frame = NSRect(x: width - 18 - 62, y: y - 2, width: 62, height: 21)
        copyBtn.toolTip = "Copy: \(command)"
        copyBtn.onClick = { btn in
            let pb = NSPasteboard.general
            pb.clearContents()
            pb.setString(command, forType: .string)
            btn.title = "Copied! ✓"
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { [weak btn] in
                btn?.title = "Copy"
            }
        }
        container.addSubview(copyBtn)
    }

    private func addPathRow(label: String, path: String, in container: NSView, y: CGFloat, width: CGFloat) {
        let lbl = NSTextField(labelWithString: label)
        lbl.font = CubeTokens.bodyLabelFont
        lbl.textColor = CubeTokens.secondaryText
        lbl.frame = NSRect(x: 18, y: y, width: 120, height: 18)
        container.addSubview(lbl)

        let pathBox = NSTextField(labelWithString: path)
        pathBox.font = NSFont.systemFont(ofSize: 11, weight: .regular)
        pathBox.textColor = CubeTokens.primaryText
        pathBox.lineBreakMode = .byTruncatingMiddle
        pathBox.frame = NSRect(x: 142, y: y, width: width - 142 - 80, height: 18)
        pathBox.toolTip = path
        container.addSubview(pathBox)

        let revealBtn = ActionButton(title: "Reveal")
        revealBtn.bezelStyle = .rounded
        revealBtn.controlSize = .small
        revealBtn.font = NSFont.systemFont(ofSize: 11)
        revealBtn.frame = NSRect(x: width - 18 - 62, y: y - 2, width: 62, height: 21)
        revealBtn.onClick = { _ in
            if FileManager.default.fileExists(atPath: path) {
                NSWorkspace.shared.selectFile(path, inFileViewerRootedAtPath: "")
            } else {
                let parentDir = URL(fileURLWithPath: path).deletingLastPathComponent().path
                NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: parentDir)
            }
        }
        container.addSubview(revealBtn)
    }

    // MARK: - Actions

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
}
