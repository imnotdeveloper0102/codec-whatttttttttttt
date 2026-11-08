# Podspec for the React Native binding (Phase 10).
#
# UNVERIFIED: not installed or built — no macOS/Xcode in this
# environment.
#
# The app adds this pod with `:path` (there is no published repo
# yet; the repository URL is set when the project is published).
# The app must also build `libcodec_whatttttttttttt.a` for its iOS
# targets (cargo build --target aarch64-apple-ios / x86_64-apple-ios
# -p codec-whatttttttttttt-c) and add the directory holding it to
# LIBRARY_SEARCH_PATHS; the podspec only points at the C header.
Pod::Spec.new do |s|
  s.name = "codec-whatttttttttttt"
  s.version = "0.1.0"
  s.summary = "React Native bindings for codec-whatttttttttttt"
  s.license = { :type => "MIT OR Apache-2.0" }
  # No homepage or source yet: the repository URL is undecided
  # (see the workspace README). Set both when the project is
  # published; until then the pod is consumed by :path only.
  s.platform = :ios, "13.0"
  s.source_files = "ios/**/*.{h,mm}"
  s.dependency "React-Core"
  s.pod_target_xcconfig = {
    # Crate include dir, relative to this podspec:
    # react-native -> packages -> typescript -> bindings ->
    # codec-whatttttttttttt -> crates/...
    "HEADER_SEARCH_PATHS" => "\"$(PODS_TARGET_SRCROOT)/../../../../crates/codec-whatttttttttttt-c/include\"",
  }
  s.requires_arc = true
end
