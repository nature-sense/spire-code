import Foundation

/// A cross-compilation platform definition (mirror of `spire_modules::Platform`).
struct Platform: Identifiable, Codable, Hashable {
    let id: String
    let name: String
    let os: String
    let architecture: PlatformArchitecture
    let toolchain: PlatformToolchain
    let sysroot: PlatformSysroot
}

struct PlatformArchitecture: Codable, Hashable {
    let cpuFamily: String
    let cpu: String
    let endian: String
    let targetTriple: String
    let march: String?

    enum CodingKeys: String, CodingKey {
        case cpuFamily = "cpu_family"
        case cpu
        case endian
        case targetTriple = "target_triple"
        case march
    }
}

struct PlatformToolchain: Codable, Hashable {
    let c: String
    let cpp: String
    let ar: String
    let strip: String
    let ld: String?
    let pkgconfig: String?
    let cArgsExtra: [String]
    let cppArgsExtra: [String]
    let linkerArgsExtra: [String]
    let needsExeWrapper: Bool

    enum CodingKeys: String, CodingKey {
        case c, cpp, ar, strip, ld, pkgconfig
        case cArgsExtra = "c_args_extra"
        case cppArgsExtra = "cpp_args_extra"
        case linkerArgsExtra = "linker_args_extra"
        case needsExeWrapper = "needs_exe_wrapper"
    }
}

struct PlatformSysroot: Codable, Hashable {
    let root: String
    let libDirs: [String]
    let includeDirs: [String]
    let pkgConfigLibdir: [String]

    enum CodingKeys: String, CodingKey {
        case root
        case libDirs = "lib_dirs"
        case includeDirs = "include_dirs"
        case pkgConfigLibdir = "pkg_config_libdir"
    }
}