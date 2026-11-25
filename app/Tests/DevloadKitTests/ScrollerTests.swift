import AppKit
import Testing
@testable import DevloadKit

/// The overlay scroller sat over the right-aligned figures ("3.74 GB")
/// while scrolling: the list shows no scroller, its fades say it scrolls.
@MainActor
@Test func noScrollerOverTheFigures() {
    let m = PopoverModel(source: SampleCore(), defaults: MemoryDefaults(), loginItem: SampleLoginItem())
    m.refresh()
    m.toggle(.apps)
    let v = Snapshots.settledView(model: m) { $0.flashScrollers() }
    let lists = Snapshots.scrollViews(v)
    #expect(lists.count == 1)
    for s in lists {
        #expect(!s.hasVerticalScroller && !s.hasHorizontalScroller)
        #expect(s.documentView!.frame.height > s.contentView.bounds.height, "the list still scrolls")
    }
}
