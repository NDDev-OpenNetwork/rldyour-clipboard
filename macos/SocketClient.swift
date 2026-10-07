import Foundation

/// A short-lived query connection, owned by the client's serial utility queue.
/// Frames are bounded before decoding; payloads stream to private transfer
/// files and can be memory-mapped for AppKit without a second full-size buffer.
private final class Connection {
    private let fd: Int32
    private var buffer = Data()
    private var next: UInt64 = 1
    private let decoder = JSONDecoder()
    private static let frameLimit = 64 * 1024
    init(path: String) throws {
        fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw ClipboardFailure(message: "Не удалось открыть локальный сокет") }
        do {
            var noPipe: Int32 = 1
            var timeout = timeval(tv_sec: 5, tv_usec: 0)
            guard setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noPipe, socklen_t(MemoryLayout<Int32>.size)) == 0,
                  setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size)) == 0,
                  setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size)) == 0,
                  fcntl(fd, F_SETFD, FD_CLOEXEC) == 0 else { throw ClipboardFailure(message: "Не удалось настроить соединение") }
            var address = sockaddr_un()
            address.sun_family = sa_family_t(AF_UNIX)
            address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
            guard path.utf8.count < MemoryLayout.size(ofValue: address.sun_path) else { throw ClipboardFailure(message: "Слишком длинный путь сокета") }
            withUnsafeMutableBytes(of: &address.sun_path) { raw in
                raw.initializeMemory(as: UInt8.self, repeating: 0)
                for (i, byte) in path.utf8.enumerated() { raw[i] = byte }
            }
            let result = withUnsafePointer(to: &address) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
                }
            }
            guard result == 0 else { throw ClipboardFailure(message: "Демон буфера пока недоступен") }
            try send(["op": "hello", "v": 1, "role": "ui", "watch": false])
            let hello = try frame()
            guard hello.ev == "hello", hello.v == 1 else { throw ClipboardFailure(message: "Несовместимая версия демона") }
        } catch { Darwin.close(fd); throw error }
    }
    deinit { Darwin.close(fd) }
    private func send(_ object: [String: Any]) throws {
        var data = try JSONSerialization.data(withJSONObject: object)
        data.append(10)
        guard data.count <= Self.frameLimit else { throw ClipboardFailure(message: "Слишком большой запрос") }
        var written = 0
        while written < data.count {
            let n = data.withUnsafeBytes { raw in Darwin.write(fd, raw.baseAddress!.advanced(by: written), data.count - written) }
            if n < 0 && errno == EINTR { continue }
            guard n > 0 else { throw ClipboardFailure(message: "Соединение прервано при отправке") }
            written += n
        }
    }
    private func read(maximum: Int) throws -> Data {
        var bytes = [UInt8](repeating: 0, count: min(maximum, 64 * 1024))
        while true {
            let n = Darwin.read(fd, &bytes, bytes.count)
            if n < 0 && errno == EINTR { continue }
            guard n > 0 else { throw ClipboardFailure(message: "Соединение прервано при чтении") }
            return Data(bytes.prefix(n))
        }
    }
    private func rawFrame() throws -> Data {
        while true {
            if let newline = buffer.firstIndex(of: 10) {
                let length = buffer.distance(from: buffer.startIndex, to: newline)
                guard length <= Self.frameLimit else { throw ClipboardFailure(message: "Слишком большой ответ") }
                let line = Data(buffer.prefix(upTo: newline))
                buffer.removeSubrange(...newline)
                return line
            }
            guard buffer.count <= Self.frameLimit else { throw ClipboardFailure(message: "Незавершённый ответ") }
            buffer.append(try read(maximum: min(4096, Self.frameLimit + 1 - buffer.count)))
        }
    }
    private func frame() throws -> ClipboardFrame {
        let value = try decoder.decode(ClipboardFrame.self, from: rawFrame())
        if value.ev == "error" { throw ClipboardFailure(message: value.message ?? "Ошибка демона") }
        return value
    }
    func request(_ op: String, fields: [String: Any] = [:]) throws -> ClipboardFrame {
        let req = next; next += 1
        var object = fields; object["op"] = op; object["req"] = req
        try send(object)
        while true { let reply = try frame(); if reply.req == req { return reply } }
    }
    func stats() throws -> ClipboardStats {
        let req = next; next += 1
        try send(["op": "stats", "req": req])
        while true {
            let raw = try rawFrame()
            let reply = try decoder.decode(ClipboardFrame.self, from: raw)
            if reply.ev == "error" { throw ClipboardFailure(message: reply.message ?? "Ошибка демона") }
            if reply.req == req { return try decoder.decode(ClipboardStats.self, from: raw) }
        }
    }
    func transfer(entry: Int64, mime: String, directory: URL) throws -> ClipboardTransfer {
        let reply = try request("fetch", fields: ["entry": entry, "mime": mime])
        guard reply.ev == "blob", let count = reply.bytes, count >= 0, count <= 512 * 1024 * 1024, let mime = reply.mime else { throw ClipboardFailure(message: "Неверный ответ с данными") }
        let url = directory.appendingPathComponent(UUID().uuidString)
        let output = Darwin.open(url.path, O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, 0o600)
        guard output >= 0 else { throw ClipboardFailure(message: "Не удалось создать временный файл") }
        defer { Darwin.close(output) }
        // Keep the file alive solely through its descriptor. A crash or failed
        // restore cannot leave clipboard payloads in a persistent temp cache.
        guard Darwin.unlink(url.path) == 0 else { throw ClipboardFailure(message: "Не удалось удалить имя временного файла") }
        var remaining = count
        while remaining > 0 {
            let data: Data
            if !buffer.isEmpty {
                let n = min(Int64(buffer.count), remaining)
                data = Data(buffer.prefix(Int(n))); buffer.removeFirst(Int(n))
            } else { data = try read(maximum: Int(min(remaining, 64 * 1024))) }
            var offset = 0
            while offset < data.count {
                let n = data.withUnsafeBytes { raw in Darwin.write(output, raw.baseAddress!.advanced(by: offset), data.count - offset) }
                if n < 0 && errno == EINTR { continue }
                guard n > 0 else { throw ClipboardFailure(message: "Запись временного файла прервана") }
                offset += n
            }
            remaining -= Int64(data.count)
        }
        if count == 0 { return ClipboardTransfer(mime: mime, data: Data()) }
        let length = Int(count)
        let mapped = Darwin.mmap(nil, length, PROT_READ, MAP_PRIVATE, output, 0)
        guard let mapped, mapped != MAP_FAILED else { throw ClipboardFailure(message: "Не удалось отобразить данные записи") }
        let data = Data(bytesNoCopy: mapped, count: length, deallocator: .custom { pointer, count in
            _ = Darwin.munmap(pointer, count)
        })
        return ClipboardTransfer(mime: mime, data: data)
    }
}

final class ClipboardClient: @unchecked Sendable {
    private let queue = DispatchQueue(label: "io.nddev.rldyour-clipboard.query", qos: .utility)
    private let path: String
    private let transfers: URL
    init(path: String? = nil) {
        let root = path.map { URL(fileURLWithPath: $0).deletingLastPathComponent() }
            ?? ProcessInfo.processInfo.environment["RLDYOUR_CLIPBOARD_HOME"].map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/rldyour-clipboard")
        self.path = path ?? root.appendingPathComponent("rldyour-clipboard.sock").path
        transfers = root.appendingPathComponent("transfers")
    }
    func page(pinned: Bool, query: String, before: Int64?, done: @escaping @Sendable (Result<ClipboardPage, Error>) -> Void) {
        queue.async {
            do {
                let connection = try Connection(path: self.path)
                let stats = try connection.stats()
                var fields: [String: Any] = ["pinned": pinned, "limit": 20]
                if !query.isEmpty { fields["query"] = query }
                if let before { fields["before"] = before }
                let reply = try connection.request("list", fields: fields)
                guard reply.ev == "list", let items = reply.items else { throw ClipboardFailure(message: "Неверный список") }
                done(.success(ClipboardPage(items: items, stats: stats, more: reply.more ?? (items.count == 20))))
            } catch { done(.failure(error)) }
        }
    }
    func pin(_ entry: ClipboardEntry, done: @escaping @Sendable (Result<Void, Error>) -> Void) {
        queue.async {
            do { _ = try Connection(path: self.path).request("pin", fields: ["entry": entry.id, "pinned": !entry.pinned]); done(.success(())) }
            catch { done(.failure(error)) }
        }
    }
    func fetch(_ entry: ClipboardEntry, done: @escaping @Sendable (Result<[ClipboardTransfer], Error>) -> Void) {
        queue.async {
            do {
                try FileManager.default.createDirectory(at: self.transfers, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
                let connection = try Connection(path: self.path)
                let mimes = ClipboardTypes.supported(entry)
                guard !mimes.isEmpty else { throw ClipboardFailure(message: "Формат записи нельзя восстановить в AppKit") }
                var files: [ClipboardTransfer] = []
                for mime in mimes { files.append(try connection.transfer(entry: entry.id, mime: mime, directory: self.transfers)) }
                done(.success(files))
            } catch { done(.failure(error)) }
        }
    }
}
