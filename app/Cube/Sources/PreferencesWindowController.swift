import Foundation
import AppKit

private class PreferencesContentView: NSView {
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
                    // Prefer non-link-local (global unicast or ULA)
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

class PreferencesWindowController: NSWindowController {
    static let shared = PreferencesWindowController()
    
    private var radioAppleDefault: NSButton!
    private var radioCustomSubnet: NSButton!
    private var radioBridged: NSButton!
    
    private var customSubnetField: NSTextField!
    private var interfacePopUp: NSPopUpButton!
    
    private var hostIPv4Label: NSTextField!
    private var hostIPv6Label: NSTextField!
    private var vmGatewayLabel: NSTextField!
    private var socketLabel: NSTextField!
    
    private var autoStartCheckbox: NSButton!
    private var launchAtLoginCheckbox: NSButton!
    
    private var lastIPv4: String = ""
    private var lastIPv6: String = ""
    
    convenience init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 590, height: 580),
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
        
        let container = PreferencesContentView(frame: NSRect(x: 0, y: 0, width: 590, height: 580))
        container.autoresizingMask = [.width, .height]
        window.contentView = container
        
        let margin: CGFloat = 24
        let contentWidth: CGFloat = 542
        var y: CGFloat = 22
        
        // --- 1. Mode Selection ---
        let headerLabel = NSTextField(labelWithString: "Network Operation Mode")
        headerLabel.font = NSFont.boldSystemFont(ofSize: 13)
        headerLabel.frame = NSRect(x: margin, y: y, width: contentWidth, height: 20)
        container.addSubview(headerLabel)
        y += 28
        
        // Option 1: Apple Default NAT
        radioAppleDefault = NSButton(radioButtonWithTitle: "Shared NAT — Apple Default (Recommended)", target: self, action: #selector(radioChanged))
        radioAppleDefault.frame = NSRect(x: margin, y: y, width: contentWidth, height: 20)
        container.addSubview(radioAppleDefault)
        y += 22
        
        let appleDefaultHint = NSTextField(wrappingLabelWithString: "Uses Apple's native subnet (192.168.64.0/24). Runs 100% unprivileged without root or sudo.")
        appleDefaultHint.font = NSFont.systemFont(ofSize: 11)
        appleDefaultHint.textColor = .secondaryLabelColor
        appleDefaultHint.maximumNumberOfLines = 2
        appleDefaultHint.lineBreakMode = .byWordWrapping
        appleDefaultHint.frame = NSRect(x: margin + 20, y: y, width: contentWidth - 20, height: 30)
        container.addSubview(appleDefaultHint)
        y += 34
        
        // Option 2: Custom Subnet NAT
        radioCustomSubnet = NSButton(radioButtonWithTitle: "Shared NAT — Custom Subnet", target: self, action: #selector(radioChanged))
        radioCustomSubnet.frame = NSRect(x: margin, y: y, width: 220, height: 20)
        container.addSubview(radioCustomSubnet)
        
        customSubnetField = NSTextField(frame: NSRect(x: margin + 225, y: y - 1, width: 170, height: 22))
        customSubnetField.placeholderString = "192.168.18.0/24"
        customSubnetField.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        container.addSubview(customSubnetField)
        y += 24
        
        let customSubnetHint = NSTextField(wrappingLabelWithString: "Specify an RFC1918 CIDR (e.g. 192.168.18.0/24). Requires a separately administered root service; unavailable in Cube.")
        customSubnetHint.font = NSFont.systemFont(ofSize: 11)
        customSubnetHint.textColor = .secondaryLabelColor
        customSubnetHint.maximumNumberOfLines = 2
        customSubnetHint.lineBreakMode = .byWordWrapping
        customSubnetHint.frame = NSRect(x: margin + 20, y: y, width: contentWidth - 20, height: 30)
        container.addSubview(customSubnetHint)
        y += 34
        
        // Option 3: Bridged LAN
        radioBridged = NSButton(radioButtonWithTitle: "Bridged LAN — Host Physical Interface", target: self, action: #selector(radioChanged))
        radioBridged.frame = NSRect(x: margin, y: y, width: 260, height: 20)
        container.addSubview(radioBridged)
        
        interfacePopUp = NSPopUpButton(frame: NSRect(x: margin + 265, y: y - 2, width: 150, height: 24), pullsDown: false)
        let ifaces = detectInterfaces()
        for iface in ifaces {
            interfacePopUp.addItem(withTitle: iface)
        }
        container.addSubview(interfacePopUp)
        y += 26
        
        let bridgedHint = NSTextField(wrappingLabelWithString: "Attaches guest directly to your physical network. Guest receives IP from local DHCP router.")
        bridgedHint.font = NSFont.systemFont(ofSize: 11)
        bridgedHint.textColor = .secondaryLabelColor
        bridgedHint.maximumNumberOfLines = 2
        bridgedHint.lineBreakMode = .byWordWrapping
        bridgedHint.frame = NSRect(x: margin + 20, y: y, width: contentWidth - 20, height: 30)
        container.addSubview(bridgedHint)
        y += 36
        
        // Separator 1
        let sep1 = NSBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 1))
        sep1.boxType = .separator
        container.addSubview(sep1)
        y += 16
        
        // --- 2. Host & VM Network Addresses ---
        let hostHeader = NSTextField(labelWithString: "Host Network Addresses")
        hostHeader.font = NSFont.boldSystemFont(ofSize: 13)
        hostHeader.frame = NSRect(x: margin, y: y, width: contentWidth, height: 18)
        container.addSubview(hostHeader)
        y += 24
        
        // Host IPv4
        let ipv4Title = NSTextField(labelWithString: "Host IPv4:")
        ipv4Title.font = NSFont.systemFont(ofSize: 12, weight: .medium)
        ipv4Title.frame = NSRect(x: margin, y: y, width: 85, height: 18)
        container.addSubview(ipv4Title)
        
        hostIPv4Label = NSTextField(labelWithString: "Detecting...")
        hostIPv4Label.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        hostIPv4Label.textColor = .labelColor
        hostIPv4Label.isSelectable = true
        hostIPv4Label.frame = NSRect(x: margin + 88, y: y + 1, width: contentWidth - 178, height: 18)
        container.addSubview(hostIPv4Label)
        
        let copyIPv4Btn = NSButton(title: "Copy", target: self, action: #selector(copyIPv4Clicked))
        copyIPv4Btn.bezelStyle = .rounded
        copyIPv4Btn.frame = NSRect(x: margin + contentWidth - 85, y: y - 4, width: 85, height: 26)
        container.addSubview(copyIPv4Btn)
        y += 26
        
        // Host IPv6
        let ipv6Title = NSTextField(labelWithString: "Host IPv6:")
        ipv6Title.font = NSFont.systemFont(ofSize: 12, weight: .medium)
        ipv6Title.frame = NSRect(x: margin, y: y, width: 85, height: 18)
        container.addSubview(ipv6Title)
        
        hostIPv6Label = NSTextField(labelWithString: "Detecting...")
        hostIPv6Label.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        hostIPv6Label.textColor = .labelColor
        hostIPv6Label.isSelectable = true
        hostIPv6Label.frame = NSRect(x: margin + 88, y: y + 1, width: contentWidth - 178, height: 18)
        container.addSubview(hostIPv6Label)
        
        let copyIPv6Btn = NSButton(title: "Copy", target: self, action: #selector(copyIPv6Clicked))
        copyIPv6Btn.bezelStyle = .rounded
        copyIPv6Btn.frame = NSRect(x: margin + contentWidth - 85, y: y - 4, width: 85, height: 26)
        container.addSubview(copyIPv6Btn)
        y += 26
        
        // VM Gateway
        let gwTitle = NSTextField(labelWithString: "VM Gateway:")
        gwTitle.font = NSFont.systemFont(ofSize: 12, weight: .medium)
        gwTitle.frame = NSRect(x: margin, y: y, width: 85, height: 18)
        container.addSubview(gwTitle)
        
        vmGatewayLabel = NSTextField(labelWithString: "192.168.64.1")
        vmGatewayLabel.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        vmGatewayLabel.textColor = .secondaryLabelColor
        vmGatewayLabel.isSelectable = true
        vmGatewayLabel.frame = NSRect(x: margin + 88, y: y + 1, width: contentWidth - 90, height: 18)
        container.addSubview(vmGatewayLabel)
        y += 32
        
        // Separator 2
        let sep2 = NSBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 1))
        sep2.boxType = .separator
        container.addSubview(sep2)
        y += 16
        
        // --- 3. Socket Information ---
        let socketHeader = NSTextField(labelWithString: "Unix Domain Socket")
        socketHeader.font = NSFont.boldSystemFont(ofSize: 13)
        socketHeader.frame = NSRect(x: margin, y: y, width: contentWidth, height: 18)
        container.addSubview(socketHeader)
        y += 22
        
        socketLabel = NSTextField(labelWithString: "")
        socketLabel.font = NSFont.monospacedSystemFont(ofSize: 11, weight: .regular)
        socketLabel.textColor = .secondaryLabelColor
        socketLabel.isSelectable = true
        socketLabel.frame = NSRect(x: margin, y: y + 2, width: contentWidth - 95, height: 18)
        container.addSubview(socketLabel)
        
        let copyBtn = NSButton(title: "Copy", target: self, action: #selector(copySocketClicked))
        copyBtn.bezelStyle = .rounded
        copyBtn.frame = NSRect(x: margin + contentWidth - 85, y: y - 4, width: 85, height: 28)
        container.addSubview(copyBtn)
        y += 34
        
        // Separator 3
        let sep3 = NSBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 1))
        sep3.boxType = .separator
        container.addSubview(sep3)
        y += 16
        
        // --- 4. Startup & Automation ---
        let startupHeader = NSTextField(labelWithString: "Startup & Automation")
        startupHeader.font = NSFont.boldSystemFont(ofSize: 13)
        startupHeader.frame = NSRect(x: margin, y: y, width: contentWidth, height: 18)
        container.addSubview(startupHeader)
        y += 24
        
        autoStartCheckbox = NSButton(checkboxWithTitle: "Automatically start network service when Cube opens", target: nil, action: nil)
        autoStartCheckbox.font = NSFont.systemFont(ofSize: 12)
        autoStartCheckbox.frame = NSRect(x: margin, y: y, width: contentWidth, height: 20)
        container.addSubview(autoStartCheckbox)
        y += 24
        
        launchAtLoginCheckbox = NSButton(checkboxWithTitle: "Launch Cube at login (menu bar app)", target: nil, action: nil)
        launchAtLoginCheckbox.font = NSFont.systemFont(ofSize: 12)
        launchAtLoginCheckbox.frame = NSRect(x: margin, y: y, width: contentWidth, height: 20)
        container.addSubview(launchAtLoginCheckbox)
        y += 30
        
        // Separator 4
        let sep4 = NSBox(frame: NSRect(x: margin, y: y, width: contentWidth, height: 1))
        sep4.boxType = .separator
        container.addSubview(sep4)
        y += 18
        
        // --- 4. Action Buttons ---
        let versionLabel = NSTextField(labelWithString: AppVersion.displayString)
        versionLabel.font = NSFont.systemFont(ofSize: 11)
        versionLabel.textColor = .tertiaryLabelColor
        versionLabel.frame = NSRect(x: margin, y: y + 6, width: 220, height: 18)
        container.addSubview(versionLabel)
        
        let cancelBtn = NSButton(title: "Cancel", target: self, action: #selector(cancelClicked))
        cancelBtn.bezelStyle = .rounded
        cancelBtn.frame = NSRect(x: margin + contentWidth - 225, y: y, width: 95, height: 32)
        cancelBtn.keyEquivalent = "\u{1b}" // ESC
        container.addSubview(cancelBtn)
        
        let applyBtn = NSButton(title: "Apply & Restart", target: self, action: #selector(applyClicked))
        applyBtn.bezelStyle = .rounded
        applyBtn.frame = NSRect(x: margin + contentWidth - 125, y: y, width: 125, height: 32)
        applyBtn.keyEquivalent = "\r" // Enter
        container.addSubview(applyBtn)
    }
    
    override func showWindow(_ sender: Any?) {
        loadCurrentState()
        super.showWindow(sender)
        NSApp.activate(ignoringOtherApps: true)
        window?.makeKeyAndOrderFront(sender)
    }
    
    private func loadCurrentState() {
        let sock = ServiceManager.shared.defaultSocketPath
        socketLabel.stringValue = sock.replacingOccurrences(of: NSHomeDirectory(), with: "~")
        
        // Refresh Host Network Information
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
        
        // Startup Settings
        autoStartCheckbox.state = ServiceManager.shared.autoStartService ? .on : .off
        launchAtLoginCheckbox.state = ServiceManager.shared.isLaunchAtLoginEnabled ? .on : .off
        
        updateControlEnablement()
    }
    
    private func gatewayForSubnet(_ cidr: String) -> String {
        let parts = cidr.components(separatedBy: "/")
        let octets = parts[0].components(separatedBy: ".")
        if octets.count == 4 {
            return "\(octets[0]).\(octets[1]).\(octets[2]).1 (Custom Subnet Gateway)"
        }
        return "192.168.64.1"
    }
    
    private func updateControlEnablement() {
        customSubnetField.isEnabled = (radioCustomSubnet.state == .on)
        interfacePopUp.isEnabled = (radioBridged.state == .on)
    }
    
    @objc private func radioChanged(_ sender: NSButton) {
        if sender == radioAppleDefault {
            radioAppleDefault.state = .on
            radioCustomSubnet.state = .off
            radioBridged.state = .off
            vmGatewayLabel.stringValue = "192.168.64.1 (Shared NAT Host Gateway)"
        } else if sender == radioCustomSubnet {
            radioAppleDefault.state = .off
            radioCustomSubnet.state = .on
            radioBridged.state = .off
            let cidr = customSubnetField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            vmGatewayLabel.stringValue = gatewayForSubnet(cidr.isEmpty ? "192.168.18.0/24" : cidr)
        } else if sender == radioBridged {
            radioAppleDefault.state = .off
            radioCustomSubnet.state = .off
            radioBridged.state = .on
            vmGatewayLabel.stringValue = "Assigned by physical LAN router"
        }
        updateControlEnablement()
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
    
    @objc private func cancelClicked() {
        window?.close()
    }
    
    @objc private func applyClicked() {
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
        
        // Save Startup Settings
        ServiceManager.shared.autoStartService = (autoStartCheckbox.state == .on)
        do {
            try ServiceManager.shared.setLaunchAtLogin(enabled: launchAtLoginCheckbox.state == .on)
        } catch {
            print("[Cube] Launch at login registration failed: \(error)")
        }
        
        window?.close()
        
        let completion: (Bool, String?) -> Void = { success, error in
            if !success, let error = error {
                let alert = NSAlert()
                alert.messageText = "Network Service Could Not Apply Settings"
                alert.informativeText = error
                alert.runModal()
            }
        }
        if ServiceManager.shared.isRunning {
            ServiceManager.shared.restartService(completion: completion)
        } else if ServiceManager.shared.autoStartService {
            ServiceManager.shared.startService(completion: completion)
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
                // Filter active physical en* interfaces
                if name.starts(with: "en") && !names.contains(name) {
                    names.append(name)
                }
                cursor = ptr.pointee.ifa_next
            }
        }
        return names.isEmpty ? ["en0"] : names.sorted()
    }
}
