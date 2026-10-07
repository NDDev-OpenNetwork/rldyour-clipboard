import AppKit
import Foundation

@MainActor private final class HistoryPanel: NSPanel {
    override var canBecomeKey: Bool { true }
}
@MainActor private final class HistoryRows: NSStackView {
    override var isFlipped: Bool { true }
}
@MainActor private final class HistoryClip: NSClipView {
    override var isFlipped: Bool { true }
}
@MainActor private final class EntryButton: NSButton {
    let entry: ClipboardEntry
    init(entry: ClipboardEntry, title: String, target: AnyObject, action: Selector) {
        self.entry = entry
        super.init(frame: .zero)
        self.title = title; self.target = target; self.action = action
    }
    required init?(coder: NSCoder) { return nil }
}

@MainActor final class ClipboardAppDelegate: NSObject, NSApplicationDelegate, NSSearchFieldDelegate, NSWindowDelegate {
    private let status = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    private let panel = HistoryPanel(contentRect: NSRect(x: 0, y: 0, width: 500, height: 560),
                                    styleMask: [.titled, .closable, .nonactivatingPanel], backing: .buffered, defer: false)
    private let search = NSSearchField()
    private let tabs = NSSegmentedControl(labels: ["Недавние", "Закреплённые"], trackingMode: .selectOne, target: nil, action: nil)
    private let rows = HistoryRows()
    private let note = NSTextField(labelWithString: "Обычные записи: 7 дней. Закреплённые: бессрочно.")
    private let more = NSButton(title: "Ещё", target: nil, action: nil)
    private var generation = 0
    private var before: Int64?
    private var debounce: DispatchWorkItem?
    private lazy var client = ClipboardClient(path: Bundle.main.object(forInfoDictionaryKey: "ClipboardSocketPath") as? String)

    func applicationDidFinishLaunching(_ notification: Notification) {
        status.button?.image = NSImage(systemSymbolName: "doc.on.clipboard", accessibilityDescription: "Буфер обмена")
        status.button?.target = self
        status.button?.action = #selector(togglePanel)
        status.button?.toolTip = "История буфера обмена · 7 дней · закрепление без срока"
        panel.title = "Буфер обмена"
        panel.isReleasedWhenClosed = false
        panel.level = .floating
        panel.delegate = self
        search.placeholderString = "Поиск в истории"
        search.delegate = self
        tabs.selectedSegment = 0
        tabs.target = self; tabs.action = #selector(changeTab)
        more.target = self; more.action = #selector(loadMore)
        rows.orientation = .vertical; rows.alignment = .leading; rows.spacing = 5
        let scroll = NSScrollView()
        scroll.contentView = HistoryClip()
        scroll.hasVerticalScroller = true; scroll.drawsBackground = false
        scroll.documentView = rows
        rows.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            rows.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor, constant: 4),
            rows.trailingAnchor.constraint(equalTo: scroll.contentView.trailingAnchor, constant: -4),
            rows.topAnchor.constraint(equalTo: scroll.contentView.topAnchor, constant: 4)
        ])
        note.font = .systemFont(ofSize: 11)
        note.textColor = .secondaryLabelColor
        note.lineBreakMode = .byWordWrapping
        note.maximumNumberOfLines = 2
        let stack = NSStackView(views: [search, tabs, scroll, more, note])
        stack.orientation = .vertical; stack.alignment = .leading; stack.spacing = 12
        let container = NSView()
        panel.contentView = container
        container.addSubview(stack)
        stack.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            container.widthAnchor.constraint(equalToConstant: 500),
            stack.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -16),
            stack.topAnchor.constraint(equalTo: container.topAnchor, constant: 16),
            stack.bottomAnchor.constraint(equalTo: container.bottomAnchor, constant: -16),
            search.widthAnchor.constraint(equalTo: stack.widthAnchor),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 360),
            note.widthAnchor.constraint(equalTo: stack.widthAnchor)
        ])
        #if CLIPBOARD_QA
        togglePanel()
        #endif
    }
    func applicationWillTerminate(_ notification: Notification) { debounce?.cancel() }
    func windowWillClose(_ notification: Notification) { generation += 1; debounce?.cancel() }

    @objc private func togglePanel() {
        if panel.isVisible { panel.close(); return }
        panel.center()
        panel.makeKeyAndOrderFront(nil)
        panel.makeFirstResponder(search)
        reload()
    }
    @objc private func changeTab() { reload() }
    func controlTextDidChange(_ notification: Notification) {
        debounce?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.reload() }
        debounce = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15, execute: work)
    }
    private func reload() {
        generation += 1
        before = nil
        for view in rows.arrangedSubviews { rows.removeArrangedSubview(view); view.removeFromSuperview() }
        fetchPage()
    }
    @objc private func loadMore() { fetchPage() }
    private func fetchPage() {
        let token = generation
        more.isEnabled = false
        client.page(pinned: tabs.selectedSegment == 1, query: search.stringValue, before: before) { [weak self] result in
            Task { @MainActor in
                guard let self, token == self.generation, self.panel.isVisible else { return }
                switch result {
                case .success(let page):
                    if page.items.isEmpty && self.rows.arrangedSubviews.isEmpty {
                        self.rows.addArrangedSubview(NSTextField(labelWithString: "Записей пока нет"))
                    }
                    for entry in page.items { self.addRow(entry) }
                    if let last = page.items.last { self.before = last.id }
                    self.more.isHidden = !page.more
                    self.more.isEnabled = true
                    let retention = page.stats.retention_days.map { "Недавние: \($0) дней" } ?? "Срок хранения отключён"
                    self.note.stringValue = "\(retention) · Закреплённые: бессрочно. Для вставки после выбора нажми ⌘V."
                case .failure(let error):
                    self.note.stringValue = error.localizedDescription
                    self.more.isEnabled = true
                }
            }
        }
    }
    private func addRow(_ entry: ClipboardEntry) {
        let copy = EntryButton(entry: entry, title: entry.caption, target: self, action: #selector(copyEntry(_:)))
        copy.isBordered = false
        copy.alignment = .left
        copy.font = .systemFont(ofSize: 13)
        copy.cell?.lineBreakMode = .byTruncatingTail
        copy.toolTip = "Скопировать запись в буфер"
        if rows.arrangedSubviews.isEmpty { copy.keyEquivalent = "\r" }
        let pin = EntryButton(entry: entry, title: "", target: self, action: #selector(pinEntry(_:)))
        pin.image = NSImage(systemSymbolName: entry.pinned ? "pin.fill" : "pin", accessibilityDescription: entry.pinned ? "Открепить" : "Закрепить")
        pin.toolTip = entry.pinned ? "Открепить: снова действует срок последнего копирования" : "Закрепить бессрочно"
        pin.setAccessibilityLabel(entry.pinned ? "Открепить \(entry.caption)" : "Закрепить \(entry.caption)")
        let row = NSStackView(views: [copy, pin])
        row.orientation = .horizontal; row.spacing = 8; row.distribution = .fill
        row.translatesAutoresizingMaskIntoConstraints = false
        rows.addArrangedSubview(row)
        NSLayoutConstraint.activate([
            row.widthAnchor.constraint(equalTo: rows.widthAnchor),
            row.heightAnchor.constraint(equalToConstant: 36),
            pin.widthAnchor.constraint(equalToConstant: 36)
        ])
        copy.setContentHuggingPriority(.defaultLow, for: .horizontal)
        copy.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    }
    @objc private func pinEntry(_ sender: EntryButton) {
        sender.isEnabled = false
        client.pin(sender.entry) { [weak self] result in
            Task { @MainActor in
                guard let self else { return }
                switch result {
                case .success: self.reload()
                case .failure(let error): self.note.stringValue = error.localizedDescription; sender.isEnabled = true
                }
            }
        }
    }
    @objc private func copyEntry(_ sender: EntryButton) {
        let token = generation
        sender.isEnabled = false
        client.fetch(sender.entry) { [weak self] result in
            Task { @MainActor in
                guard let self, token == self.generation, self.panel.isVisible else { return }
                switch result {
                case .success(let transfers):
                    do {
                        var formats: [(NSPasteboard.PasteboardType, Data)] = []
                        var fileURLs: [NSURL] = []
                        for transfer in transfers {
                            if let uti = ClipboardTypes.uti(transfer.mime) {
                                let data = transfer.data
                                if sender.entry.kind == "files", let text = String(data: data, encoding: .utf8) {
                                    fileURLs = text.components(separatedBy: .newlines).compactMap { line in
                                        guard let url = URL(string: line.trimmingCharacters(in: .whitespaces)), url.isFileURL else { return nil }
                                        return url as NSURL
                                    }
                                } else { formats.append((NSPasteboard.PasteboardType(uti), data)) }
                            }
                        }
                        guard !formats.isEmpty || !fileURLs.isEmpty else { throw ClipboardFailure(message: "Запись не содержит поддерживаемых данных") }
                        #if !CLIPBOARD_QA
                        let board = NSPasteboard.general
                        board.clearContents()
                        let marker = NSPasteboard.PasteboardType("org.nspasteboard.AutoGeneratedType")
                        board.setData(Data(), forType: marker)
                        if !fileURLs.isEmpty { board.writeObjects(fileURLs); board.setData(Data(), forType: marker) }
                        for (type, data) in formats { board.setData(data, forType: type) }
                        board.setData(Data(), forType: marker)
                        #endif
                        self.panel.close()
                    } catch { self.note.stringValue = error.localizedDescription }
                case .failure(let error): self.note.stringValue = error.localizedDescription
                }
                sender.isEnabled = true
            }
        }
    }
}

@main struct ClipboardApp {
    @MainActor static func main() {
        let app = NSApplication.shared
        let delegate = ClipboardAppDelegate()
        app.delegate = delegate
        app.setActivationPolicy(.accessory)
        app.run()
    }
}
