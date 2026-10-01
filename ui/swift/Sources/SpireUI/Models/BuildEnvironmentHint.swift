import Foundation

/// Why a build failed, when the answer is not in the application's own code.
///
/// One failure deserves to be named rather than left as a wall of shell output: there is no ESP-IDF on
/// **this machine** for `idf.py` to run. The IDF build module resolves an install itself when the process
/// has no exported environment, so reaching this point means the machine has none that works — a fact
/// about the machine, not about the application, and one the app can report: `idf_env_check` says what is
/// missing and `idf_env_fix` installs it. `make run-idf` remains the way to build in an environment a
/// person chose.
///
/// The match is a small, explicit list rather than a guess at compiler output, because the two kinds of
/// failure read nothing alike: a shell that cannot find a program says `command not found`, the process
/// runner says `os error 2`, and a compiler never says either.
enum BuildEnvironmentHint {
    /// The phrases a *missing program* produces, and nothing else does.
    private static let missingToolchain = [
        "idf.py: command not found",
        "idf.py: No such file or directory",
        "No such file or directory (os error 2)",
    ]

    /// A line to add to a failure, or `nil` when the failure is the application's own.
    static func forFailure(output: String, error: String? = nil) -> String? {
        let text = output + " " + (error ?? "")
        guard missingToolchain.contains(where: text.contains) else { return nil }
        return "This looks like this machine rather than your application: there is no ESP-IDF here for "
            + "`idf.py` to run. Ask the app to check it — `idf_env_check` reports what is missing and "
            + "`idf_env_fix` installs it — or launch with `make run-idf` (which exports one first) and "
            + "build again."
    }
}
