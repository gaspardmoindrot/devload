import AppKit
import SwiftUI

/// The popover, top to bottom: header, graph, Dev projects, Apps, Analyze.
/// Nothing in it opens anything else.
public struct PopoverView: View {
    public static let width: CGFloat = 332
    static let pad: CGFloat = 12
    /// One list row; unfolded lists scroll past `scrollRows` rows.
    static let rowHeight: CGFloat = 24
    static let scrollRows = 8
    /// Rows reach a little past the column so their hover tint has room.
    static let rowInset: CGFloat = 4

    let model: PopoverModel
    let icons: AppIcons
    let onGraphDraw: (() -> Void)?

    public init(model: PopoverModel, icons: AppIcons, onGraphDraw: (() -> Void)? = nil) {
        self.model = model
        self.icons = icons
        self.onGraphDraw = onGraphDraw
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            controls.padding(.top, 10)
            graph.padding(.top, 8)
            line.padding(.top, 10).padding(.bottom, 8)
            if model.snapshot != nil {
                lists
            }
            line.padding(.top, 6)
            bottom.padding(.top, 6)
        }
        .padding(Self.pad)
        .frame(width: Self.width)
    }

    private var line: some View { Rectangle().fill(Color(Theme.grid)).frame(height: 1) }

    // MARK: Header

    @ViewBuilder private var header: some View {
        if let s = model.snapshot {
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 0) {
                    Circle().fill(Color(Theme.pressure(s.pressure))).frame(width: 9, height: 9)
                        .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
                    Text(Format.pressureTitle(s.pressure)).font(.system(size: 13, weight: .semibold)).padding(.leading, 6)
                    Text(Format.since(s.since, now: s.time)).font(.system(size: 11)).foregroundStyle(.secondary)
                        .padding(.leading, 8)
                    Spacer(minLength: 8)
                    Text(Format.usedOfTotal(used: s.used, total: s.total))
                        .font(.system(size: 15, weight: .semibold)).monospacedDigit()
                }
                Text(Format.cpu(s.cpuPercent)).font(.system(size: 11)).foregroundStyle(.secondary).monospacedDigit()
            }
        } else {
            VStack(alignment: .leading, spacing: 2) {
                Text("Measuring…").font(.system(size: 13, weight: .semibold))
                Text("The first figures arrive within 5 seconds.").font(.system(size: 11)).foregroundStyle(.secondary)
            }
        }
    }

    // MARK: Graph

    private var controls: some View {
        HStack(spacing: 0) {
            Picker("Graph", selection: Binding(get: { model.mode }, set: { model.select($0) })) {
                ForEach(GraphMode.allCases, id: \.self) { Text($0.label).tag($0) }
            }
            .accessibilityIdentifier("mode")
            Spacer(minLength: 8)
            Picker("Period", selection: Binding(get: { model.period }, set: { model.select($0) })) {
                ForEach(Period.allCases, id: \.self) { Text($0.label).tag($0) }
            }
            .accessibilityIdentifier("period")
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .controlSize(.small)
        .fixedSize()
        .frame(maxWidth: .infinity)
    }

    /// The graph, the round times under it, then the legend on its own
    /// line: the same height whatever the period, mode or names.
    private var graph: some View {
        VStack(alignment: .leading, spacing: 0) {
            GraphRepresentable(series: model.series, onDraw: onGraphDraw)
                .frame(width: GraphView.size.width, height: GraphView.size.height)
                .overlay(alignment: .leading) { collecting }
            ZStack(alignment: .topLeading) {
                ForEach(model.ticks, id: \.at) { t in
                    Text(t.label).fixedSize()
                        .position(x: GraphView.size.width * t.at, y: Self.tickHeight / 2)
                }
            }
            .frame(width: GraphView.size.width, height: Self.tickHeight)
            .padding(.top, 2)
            .font(.system(size: 10)).foregroundStyle(.secondary).monospacedDigit()
            // Whole names when they fit; else long ones shorten, the last
            // item ("Other") never does.
            ViewThatFits(in: .horizontal) {
                legend(whole: true)
                legend(whole: false)
            }
            .frame(height: Self.tickHeight)
            .padding(.top, 4)
            .font(.system(size: 10.5)).foregroundStyle(.secondary)
            .accessibilityElement(children: .combine)
        }
    }

    static let tickHeight: CGFloat = 13

    private func legend(whole: Bool) -> some View {
        HStack(spacing: 12) {
            let items = model.legend
            ForEach(Array(items.enumerated()), id: \.offset) { i, item in
                LegendLabel(item: item, whole: whole).layoutPriority(i == items.count - 1 ? 1 : 0)
            }
            Spacer(minLength: 0)
        }
    }

    /// "Collecting since 14:02" in the empty start of the graph, when it is
    /// wide enough for the note.
    @ViewBuilder private var collecting: some View {
        if let c = model.collecting {
            let width = GraphView.size.width * c.width
            Text(c.note).font(.system(size: 10.5)).foregroundStyle(.tertiary)
                .lineLimit(1).fixedSize()
                .frame(width: width)
                .opacity(width >= 120 ? 1 : 0)
        }
    }

    // MARK: Lists

    private var lists: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Dev projects").fontWeight(.semibold)
                Spacer()
                Text("Footprint")
            }
            .font(.system(size: 11)).foregroundStyle(.secondary)
            .padding(.horizontal, Self.rowInset)
            .padding(.bottom, 2)
            let dev = model.list(.dev)
            if dev.rows.isEmpty {
                Text("No worktree or agent session in use").font(.system(size: 11)).foregroundStyle(.tertiary)
                    .padding(.horizontal, Self.rowInset)
                    .frame(height: Self.rowHeight)
            }
            rows(dev, name: "dev") { _ in
                Image(systemName: "arrow.triangle.branch").font(.system(size: 11, weight: .medium))
                    .foregroundStyle(.secondary)
            }
            MoreView(list: dev) { model.toggle(.dev) }.accessibilityIdentifier("more.dev")

            HStack(spacing: 4) {
                Text("Apps").fontWeight(.semibold)
                Text("(outside dev projects)")
            }
            .font(.system(size: 11)).foregroundStyle(.secondary)
            .padding(.horizontal, Self.rowInset)
            .padding(.top, 8).padding(.bottom, 2)
            let apps = model.list(.apps)
            rows(apps, name: "apps") { r in
                Image(nsImage: icons.icon(for: r) ?? AppIcons.generic)
                    .resizable().frame(width: 16, height: 16)
            }
            MoreView(list: apps) { model.toggle(.apps) }.accessibilityIdentifier("more.apps")
        }
        .padding(.horizontal, -Self.rowInset)
    }

    /// The rows of a list; an unfolded list scrolls past `scrollRows`.
    @ViewBuilder private func rows<Icon: View>(_ list: PopoverModel.ListView, name: String,
                                               @ViewBuilder icon: @escaping (Row) -> Icon) -> some View {
        let content = VStack(spacing: 0) {
            ForEach(list.rows, id: \.id) { r in
                RowView(row: r, model: model) { icon(r) }
            }
        }
        if list.expanded && list.rows.count > Self.scrollRows {
            ScrollingRows(rows: list.rows, model: model, name: name) { r in
                RowView(row: r, model: model) { icon(r) }
            }
        } else {
            content
        }
    }

    // MARK: Bottom

    private var bottom: some View {
        HStack {
            Button(model.copied ? "Prompt copied" : "Analyze") { model.analyze() }
                .controlSize(.small)
                .frame(minWidth: 96, alignment: .leading)
            .help("Copy a prompt for Claude Code or Codex about the period on the graph")
            Spacer()
            Menu {
                Toggle("Launch at login", isOn: Binding(get: { model.login.isOn }, set: { model.setLaunchAtLogin($0) }))
                if let note = model.login.note {
                    Text(note)
                }
                Divider()
                Button("Quit devload") { model.quit() }
            } label: {
                Text("···").font(.system(size: 13, weight: .semibold)).foregroundStyle(.secondary)
            }
            .menuStyle(.button)
            .buttonStyle(.plain)
            .menuIndicator(.hidden)
            .fixedSize()
            .accessibilityLabel("More")
        }
    }
}

struct LegendLabel: View {
    let item: PopoverModel.LegendItem
    var whole = false

    var body: some View {
        HStack(spacing: 5) {
            RoundedRectangle(cornerRadius: 2).fill(Color(item.color).opacity(item.alpha)).frame(width: 8, height: 8)
            Text(item.name).lineLimit(1).truncationMode(.tail).fixedSize(horizontal: whole, vertical: false)
        }
        .help(item.name)
    }
}

/// A row of a list. A click unfolds what it is made of; its "×" (on hover
/// or while unfolded) asks, under it, before quitting it.
struct RowView<Icon: View>: View {
    let row: Row
    let model: PopoverModel
    @ViewBuilder let icon: () -> Icon
    @State private var hover = false

    private var open: Bool { model.open == row.id }
    private var quitting: Bool { model.quitting.contains(row.id) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            line
            if open { RowDetail(row: row, model: model) }
        }
        .background(RoundedRectangle(cornerRadius: 5).fill(Color.primary.opacity(open ? 0.04 : 0)))
    }

    private var line: some View {
        HStack(spacing: 6) {
            Image(systemName: "chevron.right").font(.system(size: 8, weight: .bold)).foregroundStyle(.tertiary)
                .rotationEffect(.degrees(open ? 90 : 0))
                .frame(width: 8)
            icon().frame(width: 16, height: 16)
            Text(row.name).font(.system(size: 13)).lineLimit(1).truncationMode(.middle).padding(.leading, 2)
                .help(row.name)
            Spacer(minLength: 8)
            if quitting {
                ProgressView().controlSize(.mini).accessibilityLabel("Quitting")
            } else if !open && !hover, let note = model.notes[row.id] {
                // Folded, what a quit left behind still shows: unfold to read it.
                Image(systemName: "exclamationmark.triangle.fill").font(.system(size: 11))
                    .foregroundStyle(Color(Theme.warn))
                    .help(note)
                    .accessibilityLabel(note)
                    .accessibilityIdentifier("note.\(row.id)")
            } else if (hover || open) && model.canQuit(row) {
                Button { model.askQuit(row.id) } label: {
                    Image(systemName: "xmark.circle.fill").font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Quit")
                .accessibilityLabel("Quit")
                .accessibilityIdentifier("quit.\(row.id)")
            }
            Text(Format.footprint(row.footprint)).font(.system(size: 13)).monospacedDigit()
                .layoutPriority(1)
        }
        .opacity(quitting ? 0.45 : 1)
        .padding(.horizontal, PopoverView.rowInset)
        .frame(height: PopoverView.rowHeight)
        .background(RoundedRectangle(cornerRadius: 5).fill(Color.primary.opacity(hover && !open ? 0.06 : 0)))
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture { model.toggleRow(row.id) }
        // One element for VoiceOver: "Slack, 1.14 GB", unfolds on press,
        // and offers Quit as an action, like the "×".
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(row.name), \(Format.footprint(row.footprint))")
        .accessibilityValue(accessibilityValue)
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("row.\(row.id)")
        .accessibilityAction { model.toggleRow(row.id) }
        .accessibilityActions {
            if !quitting && model.canQuit(row) {
                Button("Quit") { model.askQuit(row.id) }
            }
        }
    }

    /// "collapsed", then what the row's icons would say: quitting, a note.
    private var accessibilityValue: String {
        var said = [open ? "expanded" : "collapsed"]
        if quitting { said.append("quitting") }
        if let note = model.notes[row.id] { said.append(note) }
        return said.joined(separator: ", ")
    }
}

/// Under an unfolded row: its parts, then the question, or what a quit
/// left behind.
struct RowDetail: View {
    let row: Row
    let model: PopoverModel
    /// Lines up with the row's name.
    static let indent: CGFloat = PopoverView.rowInset + 8 + 6 + 16 + 6 + 2
    static let partHeight: CGFloat = 20

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let p = model.detail {
                ForEach(Array(p.parts.enumerated()), id: \.offset) { _, part in
                    partLine(part)
                }
                if p.more.count > 0 {
                    HStack {
                        Text("+\(p.more.count) more")
                        Spacer()
                        Text(Format.footprint(p.more.footprint)).monospacedDigit()
                    }
                    .modifier(PartStyle(icon: false))
                }
            }
            if model.confirming == row.id, let q = model.question(row.id) {
                confirm(q)
            } else if let note = model.notes[row.id] {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Color(Theme.warn))
                    Text(note).lineLimit(1)
                    Spacer(minLength: 4)
                    if model.stubborn.contains(row.id) {
                        Button("Force Quit") { model.forceQuit(row.id) }
                            .controlSize(.small)
                            .accessibilityIdentifier("force.\(row.id)")
                    }
                }
                .font(.system(size: 11))
                .padding(.leading, Self.indent).padding(.trailing, PopoverView.rowInset)
                .frame(minHeight: 26)
            }
        }
        .padding(.bottom, 4)
    }

    private func partLine(_ part: Part) -> some View {
        // Its icon under the row's icon, its name under the row's name.
        HStack(spacing: 8) {
            Image(systemName: Self.symbol(part.kind)).font(.system(size: 9)).frame(width: 16)
            Text(Format.part(part)).lineLimit(1).truncationMode(.middle)
            let detail = Format.partDetail(part)
            if !detail.isEmpty {
                Text(detail).foregroundStyle(.tertiary).lineLimit(1).truncationMode(.tail)
            }
            Spacer(minLength: 8)
            Text(Format.footprint(part.footprint)).monospacedDigit().layoutPriority(1)
        }
        .modifier(PartStyle(icon: true))
    }

    static func symbol(_ k: Part.Kind) -> String {
        switch k {
        case .process: "gearshape"
        case .agent: "sparkle"
        case .docker: "shippingbox"
        }
    }

    /// The question names the row; where the whole of it does not fit,
    /// "Quit <name>?", the name shortened in its middle if need be.
    private func confirm(_ question: String) -> some View {
        HStack(spacing: 6) {
            ViewThatFits(in: .horizontal) {
                Text(question).fixedSize()
                Text(Format.quitApp(row.name)).truncationMode(.middle)
            }
            .font(.system(size: 11, weight: .medium)).lineLimit(1)
            .help(question)
            .accessibilityLabel(question)
            Spacer(minLength: 4)
            Button("Cancel") { model.cancelQuit() }
                .keyboardShortcut(.cancelAction)
                .accessibilityIdentifier("quit.cancel")
            Button("Quit") { model.confirmQuit() }
                .buttonStyle(.borderedProminent)
                .tint(.red)
                .accessibilityIdentifier("quit.confirm")
        }
        .controlSize(.small)
        .padding(.leading, Self.indent).padding(.trailing, PopoverView.rowInset)
        .frame(height: 28)
    }
}

private struct PartStyle: ViewModifier {
    /// Starts with a 16 pt icon and 8 pt of space.
    let icon: Bool

    func body(content: Content) -> some View {
        content.font(.system(size: 11)).foregroundStyle(.secondary)
            .padding(.leading, RowDetail.indent - (icon ? 24 : 0)).padding(.trailing, PopoverView.rowInset)
            .frame(height: RowDetail.partHeight)
    }
}

/// "+N more" under a folded list (a click unfolds it), "Show less" under an
/// unfolded one, and a plain line for what the core did not send.
struct MoreView: View {
    let list: PopoverModel.ListView
    let toggle: () -> Void

    var body: some View {
        if list.foldable {
            Button(action: toggle) {
                HStack(spacing: 4) {
                    Text(list.expanded ? "Show less" : Format.more(list.more))
                    Image(systemName: list.expanded ? "chevron.up" : "chevron.down").font(.system(size: 8, weight: .semibold))
                    Spacer()
                    if !list.expanded { Text(Format.footprint(list.more.footprint)).monospacedDigit() }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .modifier(MoreStyle())
            .accessibilityLabel(list.expanded ? "Show less" : "Show \(list.more.count) more")
            if list.expanded && list.more.count > 0 { plain(list.more) }
        } else if list.more.count > 0 {
            plain(list.more)
        }
    }

    private func plain(_ more: More) -> some View {
        HStack {
            Text(Format.more(more))
            Spacer()
            Text(Format.footprint(more.footprint)).monospacedDigit()
        }
        .modifier(MoreStyle())
    }
}

private struct MoreStyle: ViewModifier {
    func body(content: Content) -> some View {
        content.font(.system(size: 11)).foregroundStyle(.secondary)
            .padding(.leading, RowDetail.indent).padding(.trailing, PopoverView.rowInset).frame(height: 20)
    }
}

struct GraphRepresentable: NSViewRepresentable {
    let series: Series?
    let onDraw: (() -> Void)?

    func makeNSView(context: Context) -> GraphView {
        let v = GraphView()
        v.onDraw = onDraw
        return v
    }

    func updateNSView(_ v: GraphView, context: Context) {
        v.series = series
    }
}
