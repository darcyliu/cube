import Foundation
import Darwin

final class SystemMetrics {
    static let shared = SystemMetrics()
    
    // Store previous ticks for host CPU load delta calculation
    private var previousHostTicks = host_cpu_load_info()
    private var hasSampledHostTicks = false
    
    // Store previous process CPU times: pid -> (sampleTime, cpuSec)
    private var previousProcessCPUTimes: [pid_t: (timestamp: Date, cpuSec: Double)] = [:]
    
    struct HostStats {
        let cpuPercent: Double
        let usedMemoryBytes: UInt64
        let totalMemoryBytes: UInt64
        
        var memoryPercent: Double {
            guard totalMemoryBytes > 0 else { return 0 }
            return (Double(usedMemoryBytes) / Double(totalMemoryBytes)) * 100.0
        }
        
        var formattedMemory: String {
            let usedGB = Double(usedMemoryBytes) / (1024 * 1024 * 1024)
            let totalGB = Double(totalMemoryBytes) / (1024 * 1024 * 1024)
            return String(format: "%.1f / %.0f GB", usedGB, totalGB)
        }
        
        var formattedCPU: String {
            return String(format: "%.1f%%", cpuPercent)
        }
    }
    
    struct ProcessStats {
        let pid: pid_t
        let residentMemoryMB: Double
        let cpuPercent: Double
        let threadCount: Int
        
        var formattedMemory: String {
            if residentMemoryMB >= 1024 {
                return String(format: "%.2f GB", residentMemoryMB / 1024.0)
            } else {
                return String(format: "%.0f MB", residentMemoryMB)
            }
        }
        
        var formattedCPU: String {
            return String(format: "%.1f%%", cpuPercent)
        }
    }
    
    init() {
        if let ticks = getHostCPUTicks() {
            previousHostTicks = ticks
            hasSampledHostTicks = true
        }
    }
    
    private func getHostCPUTicks() -> host_cpu_load_info? {
        var load = host_cpu_load_info()
        var count = mach_msg_type_number_t(MemoryLayout<host_cpu_load_info>.size / MemoryLayout<integer_t>.size)
        let kerr = withUnsafeMutablePointer(to: &load) { ptr in
            ptr.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                host_statistics(mach_host_self(), HOST_CPU_LOAD_INFO, $0, &count)
            }
        }
        guard kerr == KERN_SUCCESS else { return nil }
        return load
    }
    
    func sampleHost() -> HostStats {
        let totalMemory = ProcessInfo.processInfo.physicalMemory
        var usedMemory: UInt64 = 0
        
        var vmStat = vm_statistics64()
        var count = mach_msg_type_number_t(MemoryLayout<vm_statistics64>.size / MemoryLayout<integer_t>.size)
        let kerr = withUnsafeMutablePointer(to: &vmStat) { ptr in
            ptr.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                host_statistics64(mach_host_self(), HOST_VM_INFO64, $0, &count)
            }
        }
        if kerr == KERN_SUCCESS {
            let pageSize = UInt64(vm_kernel_page_size)
            usedMemory = (UInt64(vmStat.active_count) + UInt64(vmStat.wire_count) + UInt64(vmStat.compressor_page_count)) * pageSize
        }
        
        var cpuPercent: Double = 0.0
        if let currentTicks = getHostCPUTicks() {
            if hasSampledHostTicks {
                let user = currentTicks.cpu_ticks.0 >= previousHostTicks.cpu_ticks.0 ? currentTicks.cpu_ticks.0 - previousHostTicks.cpu_ticks.0 : 0
                let sys = currentTicks.cpu_ticks.1 >= previousHostTicks.cpu_ticks.1 ? currentTicks.cpu_ticks.1 - previousHostTicks.cpu_ticks.1 : 0
                let idle = currentTicks.cpu_ticks.2 >= previousHostTicks.cpu_ticks.2 ? currentTicks.cpu_ticks.2 - previousHostTicks.cpu_ticks.2 : 0
                let nice = currentTicks.cpu_ticks.3 >= previousHostTicks.cpu_ticks.3 ? currentTicks.cpu_ticks.3 - previousHostTicks.cpu_ticks.3 : 0
                
                let total = user + sys + idle + nice
                if total > 0 {
                    let active = user + sys + nice
                    cpuPercent = (Double(active) / Double(total)) * 100.0
                }
            }
            previousHostTicks = currentTicks
            hasSampledHostTicks = true
        }
        
        return HostStats(cpuPercent: cpuPercent, usedMemoryBytes: usedMemory, totalMemoryBytes: totalMemory)
    }
    
    func sampleProcess(pid: pid_t) -> ProcessStats? {
        var info = proc_taskinfo()
        let size = Int32(MemoryLayout<proc_taskinfo>.stride)
        let written = proc_pidinfo(pid, PROC_PIDTASKINFO, 0, &info, size)
        guard written == size else { return nil }
        
        var residentMB = Double(info.pti_resident_size) / (1024 * 1024)
        
        // Query physical footprint (matching macOS Activity Monitor, discounting clean shared frameworks & dyld shared cache)
        var ru = rusage_info_v4()
        let ruRet = withUnsafeMutablePointer(to: &ru) { ptr in
            ptr.withMemoryRebound(to: (rusage_info_t?).self, capacity: 1) {
                proc_pid_rusage(pid, RUSAGE_INFO_V4, $0)
            }
        }
        if ruRet == 0 && ru.ri_phys_footprint > 0 {
            residentMB = Double(ru.ri_phys_footprint) / (1024 * 1024)
        }
        
        let cpuSec = Double(info.pti_total_user + info.pti_total_system) / 1_000_000_000.0
        let now = Date()
        
        var cpuPercent: Double = 0.0
        if let prev = previousProcessCPUTimes[pid] {
            let timeDelta = now.timeIntervalSince(prev.timestamp)
            if timeDelta > 0.1 {
                let cpuDelta = cpuSec - prev.cpuSec
                if cpuDelta >= 0 {
                    cpuPercent = min(100.0 * Double(ProcessInfo.processInfo.activeProcessorCount), (cpuDelta / timeDelta) * 100.0)
                }
            }
        }
        previousProcessCPUTimes[pid] = (now, cpuSec)
        
        // Cleanup PIDs no longer active if dictionary gets large
        if previousProcessCPUTimes.count > 30 {
            previousProcessCPUTimes.removeAll()
        }
        
        return ProcessStats(
            pid: pid,
            residentMemoryMB: residentMB,
            cpuPercent: max(0.0, cpuPercent),
            threadCount: Int(info.pti_threadnum)
        )
    }
}
