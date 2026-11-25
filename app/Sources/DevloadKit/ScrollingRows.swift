import SwiftUI

/// What an unfolded list hides: rows cut off above and below the visible
/// part. Its edges fade where it does.
struct ScrollHint: Equatable {
    var above = 0
    var below = 0

    /// A row counts as hidden as soon as a part of it is out of view.
    static func of(rows: [(id: UInt32, top: CGFloat, bottom: CGFloat)], visible: ClosedRange<CGFloat>) -> ScrollHint {
        var h = ScrollHint()
        for r in rows {
            if r.top < visible.lowerBound - 0.5 { h.above += 1 }
            if r.bottom > visible.upperBound + 0.5 { h.below += 1 }
        }
        return h
    }
}

/// An unfolded list longer than `PopoverView.scrollRows`. It scrolls, and
/// shows it: the last row is cut in half and the edges fade where rows
/// are hidden.
struct ScrollingRows<Content: View>: View {
    let rows: [Row]
    let model: PopoverModel
    let name: String
    @ViewBuilder let content: (Row) -> Content

    /// Seven rows and a half: the cut row says the list goes on.
    static var height: CGFloat { PopoverView.rowHeight * (CGFloat(PopoverView.scrollRows) - 0.5) }
    static var fade: CGFloat { 14 }
    static var gap: CGFloat { 4 }

    @State private var spans: [UInt32: ClosedRange<CGFloat>] = [:]
    @State private var visible: ClosedRange<CGFloat> = 0...Self.height

    private var hint: ScrollHint {
        ScrollHint.of(rows: rows.compactMap { r in spans[r.id].map { (r.id, $0.lowerBound, $0.upperBound) } },
                      visible: visible)
    }

    var body: some View {
        let hint = hint
        ScrollViewReader { proxy in
            ScrollView(.vertical) {
                VStack(spacing: 0) {
                    ForEach(rows, id: \.id) { r in
                        content(r).modifier(Span(space: name) { spans[r.id] = $0 })
                    }
                }
                .coordinateSpace(.named(name))
            }
            // No scroller: the overlay one sat over the right-aligned
            // figures while scrolling, and the fades already say more rows
            // are hidden. The wheel and trackpad still scroll.
            .scrollIndicators(.never)
            .onScrollGeometryChange(for: ClosedRange<CGFloat>.self) { g in
                g.visibleRect.minY...g.visibleRect.maxY
            } action: { _, now in visible = now }
            .frame(height: Self.height)
            .mask {
                VStack(spacing: 0) {
                    LinearGradient(colors: [.black.opacity(hint.above > 0 ? 0 : 1), .black], startPoint: .top, endPoint: .bottom)
                        .frame(height: Self.fade)
                    Color.black
                    LinearGradient(colors: [.black, .black.opacity(hint.below > 0 ? 0 : 1)], startPoint: .top, endPoint: .bottom)
                        .frame(height: Self.fade)
                }
            }
            // Clear of the list's header when a row is cut at the top.
            .padding(.top, Self.gap)
            // An unfolded row is scrolled into view; its question, at its
            // foot, once the row has grown to hold it.
            .onChange(of: model.open) { _, id in
                guard let id, rows.contains(where: { $0.id == id }) else { return }
                withAnimation(.easeOut(duration: 0.15)) { proxy.scrollTo(id) }
            }
            .onChange(of: model.confirming) { _, id in
                guard let id, rows.contains(where: { $0.id == id }) else { return }
                DispatchQueue.main.async {
                    withAnimation(.easeOut(duration: 0.15)) { proxy.scrollTo(id, anchor: .bottom) }
                }
            }
        }
    }
}

/// Reports where a row sits in its list (top...bottom, in `space`).
private struct Span: ViewModifier {
    let space: String
    let report: (ClosedRange<CGFloat>) -> Void

    func body(content: Content) -> some View {
        content.onGeometryChange(for: ClosedRange<CGFloat>.self) { [space] g in
            let f = g.frame(in: .named(space))
            return f.minY...f.maxY
        } action: { report($0) }
    }
}
