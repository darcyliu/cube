import Foundation
import AppKit

struct OSImagePreset {
    let id: String
    let name: String
    let version: String
    let filename: String
    let downloadUrl: String
    let approximateSize: String
    let notes: String
}

struct OSImageItem {
    let id: String
    let title: String
    let filename: String
    let downloadUrl: String?
    let isPreset: Bool
    var fileSize: Int64?
    var isDownloading: Bool = false
    var downloadProgress: Double = 0.0
    
    var isCached: Bool {
        guard let size = fileSize else { return false }
        return size > 10 * 1024 * 1024 // At least 10 MB
    }
    
    var sizeDisplay: String {
        if let bytes = fileSize, bytes > 0 {
            return ImageCacheManager.formatBytes(bytes)
        }
        return "Not downloaded"
    }
}

class ImageCacheManager: NSObject, URLSessionDownloadDelegate {
    static let shared = ImageCacheManager()
    
    let cacheDir: String

    private let legacyCacheDir: String?
    
    let presets: [OSImagePreset] = [
        OSImagePreset(
            id: "freebsd-14.4",
            name: "FreeBSD 14.4-RELEASE (ARM64)",
            version: "14.4",
            filename: "FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso",
            downloadUrl: "https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/14.4/FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso",
            approximateSize: "1.14 GB",
            notes: "Standard 14-STABLE production release (distsets)"
        ),
        OSImagePreset(
            id: "freebsd-14.5",
            name: "FreeBSD 14.5-RELEASE (ARM64)",
            version: "14.5",
            filename: "FreeBSD-14.5-RELEASE-arm64-aarch64-disc1.iso",
            downloadUrl: "https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/14.5/FreeBSD-14.5-RELEASE-arm64-aarch64-disc1.iso",
            approximateSize: "1.15 GB",
            notes: "Current 14-STABLE release"
        ),
        OSImagePreset(
            id: "freebsd-15.1",
            name: "FreeBSD 15.1-RELEASE (ARM64)",
            version: "15.1",
            filename: "FreeBSD-15.1-RELEASE-arm64-aarch64-disc1.iso",
            downloadUrl: "https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/15.1/FreeBSD-15.1-RELEASE-arm64-aarch64-disc1.iso",
            approximateSize: "1.20 GB",
            notes: "15-CURRENT release (pkgbase unattended)"
        )
    ]
    
    private var activeTasks: [String: URLSessionDownloadTask] = [:]
    private var taskToPresetId: [Int: String] = [:]
    private(set) var downloadProgressMap: [String: Double] = [:]
    private var lastProgressReportTime: [String: Date] = [:]
    
    var onProgressChanged: ((String, Double) -> Void)?
    var onDownloadFinished: ((String, Bool, String?) -> Void)?
    var onCacheUpdated: (() -> Void)?
    
    private lazy var session: URLSession = {
        let config = URLSessionConfiguration.default
        return URLSession(configuration: config, delegate: self, delegateQueue: .main)
    }()
    
    override convenience init() {
        self.init(cacheDirectory: "\(NSHomeDirectory())/Library/Caches/Cube",
                  legacyCacheDirectory: "\(NSHomeDirectory())/Library/Caches/vmm")
    }

    init(cacheDirectory: String, legacyCacheDirectory: String? = nil) {
        cacheDir = cacheDirectory
        legacyCacheDir = legacyCacheDirectory
        super.init()
        ensureCacheDirectoryExists()
    }
    
    func ensureCacheDirectoryExists() {
        let fm = FileManager.default
        try? fm.createDirectory(atPath: cacheDir, withIntermediateDirectories: true)
        migrateLegacyCacheIfNeeded()
    }
    
    private func migrateLegacyCacheIfNeeded() {
        guard let legacyCacheDir = legacyCacheDir else { return }
        let fm = FileManager.default
        var isDir: ObjCBool = false
        guard fm.fileExists(atPath: legacyCacheDir, isDirectory: &isDir), isDir.boolValue else {
            return
        }
        
        // Check if legacyCacheDir is already a symlink pointing to cacheDir
        if let dest = try? fm.destinationOfSymbolicLink(atPath: legacyCacheDir) {
            if dest == cacheDir || dest == "Cube" {
                return
            }
        }
        
        // Migrate files from legacyCacheDir to cacheDir
        if let contents = try? fm.contentsOfDirectory(atPath: legacyCacheDir) {
            for item in contents {
                let oldPath = "\(legacyCacheDir)/\(item)"
                let newPath = "\(cacheDir)/\(item)"
                if !fm.fileExists(atPath: newPath) {
                    try? fm.moveItem(atPath: oldPath, toPath: newPath)
                }
            }
        }
        
        // If legacy directory is now empty, remove it and create a symlink to cacheDir for compatibility
        if let remaining = try? fm.contentsOfDirectory(atPath: legacyCacheDir), remaining.isEmpty {
            try? fm.removeItem(atPath: legacyCacheDir)
            try? fm.createSymbolicLink(atPath: legacyCacheDir, withDestinationPath: cacheDir)
        }
    }
    
    // MARK: - Query & Scan
    
    func getAllImageItems() -> [OSImageItem] {
        ensureCacheDirectoryExists()
        var items: [OSImageItem] = []
        var foundFilenames = Set<String>()
        
        // 1. Process presets
        for preset in presets {
            let path = "\(cacheDir)/\(preset.filename)"
            let exists = FileManager.default.fileExists(atPath: path)
            var size: Int64? = nil
            if exists, let attr = try? FileManager.default.attributesOfItem(atPath: path),
               let num = attr[.size] as? NSNumber {
                size = num.int64Value
            }
            
            let isDownloading = activeTasks[preset.id] != nil
            let progress = downloadProgressMap[preset.id] ?? 0.0
            
            items.append(OSImageItem(
                id: preset.id,
                title: preset.name,
                filename: preset.filename,
                downloadUrl: preset.downloadUrl,
                isPreset: true,
                fileSize: size,
                isDownloading: isDownloading,
                downloadProgress: progress
            ))
            foundFilenames.insert(preset.filename)
        }
        
        // 2. Discover additional files in cache (custom ISOs or versions)
        if let entries = try? FileManager.default.contentsOfDirectory(atPath: cacheDir) {
            for file in entries.sorted() {
                if file.hasPrefix(".") || file.hasSuffix(".partial") { continue }
                if foundFilenames.contains(file) { continue }
                
                let lower = file.lowercased()
                if lower.hasSuffix(".iso") || lower.hasSuffix(".img") || lower.hasSuffix(".raw") || lower.hasSuffix(".qcow2") {
                    let path = "\(cacheDir)/\(file)"
                    var size: Int64? = nil
                    if let attr = try? FileManager.default.attributesOfItem(atPath: path),
                       let num = attr[.size] as? NSNumber {
                        size = num.int64Value
                    }
                    
                    let cleanTitle = formatCleanTitle(filename: file)
                    items.append(OSImageItem(
                        id: file,
                        title: cleanTitle,
                        filename: file,
                        downloadUrl: nil,
                        isPreset: false,
                        fileSize: size,
                        isDownloading: false,
                        downloadProgress: 0.0
                    ))
                }
            }
        }
        
        return items
    }
    
    func getTotalCacheBytes() -> Int64 {
        ensureCacheDirectoryExists()
        var total: Int64 = 0
        if let entries = try? FileManager.default.contentsOfDirectory(atPath: cacheDir) {
            for file in entries {
                let path = "\(cacheDir)/\(file)"
                if let attr = try? FileManager.default.attributesOfItem(atPath: path),
                   let num = attr[.size] as? NSNumber {
                    total += num.int64Value
                }
            }
        }
        return total
    }
    
    static func formatBytes(_ bytes: Int64) -> String {
        let b = Double(bytes)
        if b >= 1024 * 1024 * 1024 {
            return String(format: "%.2f GB", b / (1024 * 1024 * 1024))
        } else if b >= 1024 * 1024 {
            return String(format: "%.1f MB", b / (1024 * 1024))
        } else if b >= 1024 {
            return String(format: "%.0f KB", b / 1024)
        } else {
            return "\(bytes) B"
        }
    }
    
    private func formatCleanTitle(filename: String) -> String {
        var name = filename
        if let dotIndex = name.lastIndex(of: ".") {
            name = String(name[..<dotIndex])
        }
        name = name.replacingOccurrences(of: "-arm64-aarch64", with: "")
        name = name.replacingOccurrences(of: "-aarch64", with: "")
        return name
    }
    
    // MARK: - Actions: Download / Cancel / Delete
    
    func startDownload(presetId: String) {
        guard activeTasks[presetId] == nil,
              let preset = presets.first(where: { $0.id == presetId }),
              let url = URL(string: preset.downloadUrl) else { return }
        
        ensureCacheDirectoryExists()
        let task = session.downloadTask(with: url)
        activeTasks[presetId] = task
        taskToPresetId[task.taskIdentifier] = presetId
        downloadProgressMap[presetId] = 0.0
        task.resume()
        
        onProgressChanged?(presetId, 0.0)
    }
    
    func cancelDownload(presetId: String) {
        if let task = activeTasks[presetId] {
            task.cancel()
            taskToPresetId.removeValue(forKey: task.taskIdentifier)
            activeTasks.removeValue(forKey: presetId)
            downloadProgressMap.removeValue(forKey: presetId)
            onProgressChanged?(presetId, 0.0)
            onCacheUpdated?()
        }
    }
    
    func deleteImage(filename: String) -> Bool {
        let path = "\(cacheDir)/\(filename)"
        do {
            if FileManager.default.fileExists(atPath: path) {
                try FileManager.default.removeItem(atPath: path)
            }
            // Also clean up any matching .partial
            let partial = "\(cacheDir)/\(filename).partial"
            if FileManager.default.fileExists(atPath: partial) {
                try? FileManager.default.removeItem(atPath: partial)
            }
            onCacheUpdated?()
            return true
        } catch {
            return false
        }
    }
    
    func clearAllCache() -> (count: Int, reclaimedBytes: Int64) {
        var count = 0
        var bytes: Int64 = 0
        if let entries = try? FileManager.default.contentsOfDirectory(atPath: cacheDir) {
            for file in entries {
                let path = "\(cacheDir)/\(file)"
                if let attr = try? FileManager.default.attributesOfItem(atPath: path),
                   let num = attr[.size] as? NSNumber {
                    bytes += num.int64Value
                }
                do {
                    try FileManager.default.removeItem(atPath: path)
                    count += 1
                } catch {}
            }
        }
        onCacheUpdated?()
        return (count, bytes)
    }
    
    func importLocalISO(from sourceURL: URL) throws -> String {
        ensureCacheDirectoryExists()
        let filename = sourceURL.lastPathComponent
        let destPath = "\(cacheDir)/\(filename)"
        let destURL = URL(fileURLWithPath: destPath)
        
        let fm = FileManager.default
        let resolvedSource = sourceURL.resolvingSymlinksInPath()
        if fm.fileExists(atPath: destPath) {
            let sourceAttributes = try fm.attributesOfItem(atPath: resolvedSource.path)
            let destinationAttributes = try fm.attributesOfItem(atPath: destURL.resolvingSymlinksInPath().path)
            if let sourceDevice = sourceAttributes[.systemNumber] as? NSNumber,
               let sourceFile = sourceAttributes[.systemFileNumber] as? NSNumber,
               sourceDevice == destinationAttributes[.systemNumber] as? NSNumber,
               sourceFile == destinationAttributes[.systemFileNumber] as? NSNumber {
                return filename
            }
        }

        // Stage the complete copy on the same volume before replacing any cached image.
        let stagedURL = URL(fileURLWithPath: cacheDir).appendingPathComponent(".import-\(UUID().uuidString)")
        defer { try? fm.removeItem(at: stagedURL) }
        try fm.copyItem(at: resolvedSource, to: stagedURL)
        if fm.fileExists(atPath: destPath) {
            _ = try fm.replaceItemAt(destURL, withItemAt: stagedURL)
        } else {
            try fm.moveItem(at: stagedURL, to: destURL)
        }
        onCacheUpdated?()
        return filename
    }
    
    // MARK: - URLSessionDownloadDelegate
    
    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didWriteData bytesWritten: Int64, totalBytesWritten: Int64, totalBytesExpectedToWrite: Int64) {
        guard let presetId = taskToPresetId[downloadTask.taskIdentifier] else { return }
        let progress = totalBytesExpectedToWrite > 0 ? max(0.0, min(1.0, Double(totalBytesWritten) / Double(totalBytesExpectedToWrite))) : 0.0
        downloadProgressMap[presetId] = progress
        
        let now = Date()
        let lastReport = lastProgressReportTime[presetId] ?? Date.distantPast
        if now.timeIntervalSince(lastReport) >= 0.25 || progress >= 1.0 {
            lastProgressReportTime[presetId] = now
            DispatchQueue.main.async {
                self.onProgressChanged?(presetId, progress)
            }
        }
    }
    
    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        guard let presetId = taskToPresetId[downloadTask.taskIdentifier],
              let preset = presets.first(where: { $0.id == presetId }) else { return }
        
        // Verify HTTP status
        if let http = downloadTask.response as? HTTPURLResponse, http.statusCode != 200 {
            let errorMsg = "HTTP \(http.statusCode): The server could not provide the requested ISO file."
            activeTasks.removeValue(forKey: presetId)
            taskToPresetId.removeValue(forKey: downloadTask.taskIdentifier)
            downloadProgressMap.removeValue(forKey: presetId)
            lastProgressReportTime.removeValue(forKey: presetId)
            DispatchQueue.main.async {
                self.onDownloadFinished?(presetId, false, errorMsg)
                self.onCacheUpdated?()
            }
            return
        }
        
        let destUrl = URL(fileURLWithPath: "\(cacheDir)/\(preset.filename)")
        do {
            ensureCacheDirectoryExists()
            if FileManager.default.fileExists(atPath: destUrl.path) {
                try FileManager.default.removeItem(at: destUrl)
            }
            try FileManager.default.moveItem(at: location, to: destUrl)
            
            // Verify downloaded file is valid (> 10MB)
            if let attr = try? FileManager.default.attributesOfItem(atPath: destUrl.path),
               let size = (attr[.size] as? NSNumber)?.int64Value, size < 10 * 1024 * 1024 {
                try? FileManager.default.removeItem(at: destUrl)
                throw NSError(domain: "Cube", code: 1, userInfo: [NSLocalizedDescriptionKey: "Downloaded file is incomplete or corrupted (\(size) bytes)."])
            }
            
            activeTasks.removeValue(forKey: presetId)
            taskToPresetId.removeValue(forKey: downloadTask.taskIdentifier)
            downloadProgressMap.removeValue(forKey: presetId)
            lastProgressReportTime.removeValue(forKey: presetId)
            
            DispatchQueue.main.async {
                self.onDownloadFinished?(presetId, true, nil)
                self.onCacheUpdated?()
            }
        } catch {
            activeTasks.removeValue(forKey: presetId)
            taskToPresetId.removeValue(forKey: downloadTask.taskIdentifier)
            downloadProgressMap.removeValue(forKey: presetId)
            lastProgressReportTime.removeValue(forKey: presetId)
            DispatchQueue.main.async {
                self.onDownloadFinished?(presetId, false, error.localizedDescription)
                self.onCacheUpdated?()
            }
        }
    }
    
    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard let presetId = taskToPresetId[task.taskIdentifier] else { return }
        activeTasks.removeValue(forKey: presetId)
        taskToPresetId.removeValue(forKey: task.taskIdentifier)
        downloadProgressMap.removeValue(forKey: presetId)
        lastProgressReportTime.removeValue(forKey: presetId)
        
        if let err = error, (err as NSError).code != NSURLErrorCancelled {
            DispatchQueue.main.async {
                self.onDownloadFinished?(presetId, false, err.localizedDescription)
                self.onCacheUpdated?()
            }
        }
    }
}
