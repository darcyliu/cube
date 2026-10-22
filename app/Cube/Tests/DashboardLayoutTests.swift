import AppKit

@main
struct DashboardLayoutTests {
    @MainActor
    static func main() {
        _ = NSApplication.shared
        let controller = DashboardWindowController()
        let window = controller.window!
        let root = window.contentView!
        let split = root.subviews.compactMap { $0 as? NSSplitView }.first!
        precondition(split.arrangedSubviews.count == 2)
        let sidebar = split.arrangedSubviews[0]
        let detail = split.arrangedSubviews[1]
        let sidebarScroll = sidebar.subviews.compactMap { $0 as? NSScrollView }.first!

        func checkLayout() {
            root.layoutSubtreeIfNeeded()
            precondition(!sidebar.isHidden && !split.isSubviewCollapsed(sidebar))
            precondition(sidebar.frame.width >= 199 && sidebar.frame.width <= 341)
            precondition(detail.frame.width >= 499)
            precondition(sidebar.frame.height > 300)
            precondition(abs(sidebar.frame.height - split.bounds.height) < 1)
            precondition(abs(sidebar.frame.minX) < 1)
            precondition(abs(detail.frame.minX - sidebar.frame.maxX - split.dividerThickness) < 1)
            precondition(abs(detail.frame.maxX - split.bounds.width) < 1)
            precondition(!sidebarScroll.hasHorizontalScroller)
            precondition(sidebarScroll.horizontalScrollElasticity == .none)
            precondition(sidebarScroll.documentView!.frame.width <= sidebarScroll.contentView.bounds.width + 1,
                         "Sidebar table overflows horizontally")
        }

        checkLayout()
        precondition(abs(sidebar.frame.width - 240) < 1, "Initial sidebar width")
        for width in [1000.0, 1200.0, 960.0] {
            window.setContentSize(NSSize(width: width, height: 580))
            checkLayout()
            precondition(abs(sidebar.frame.width - 240) < 1, "Resize to \(width) changed sidebar width to \(sidebar.frame.width)")
        }
        // Give the detail controls enough room to exercise the full divider range.
        window.setContentSize(NSSize(width: 1200, height: 580))
        checkLayout()
        for position in [210.0, 240.0, 300.0, 340.0] {
            split.setPosition(position, ofDividerAt: 0)
            checkLayout()
            precondition(abs(sidebar.frame.width - position) < 1, "Divider requested \(position), got \(sidebar.frame.width)")
        }
        window.setContentSize(NSSize(width: 820, height: 480))
        checkLayout()
        print("PASS: dashboard sidebar is visible at startup, after resizing, and after divider movement")
    }
}
