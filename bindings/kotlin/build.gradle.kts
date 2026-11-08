plugins {
    kotlin("jvm") version "1.3.72"
}

group = "dev.codecwhat"
version = "0.1.0"

repositories {
    mavenCentral()
}

// The JNI library is built from src/main/cpp/jni_bridge.cpp and
// linked against the Rust static library. It must be built before
// the tests run; the C crate is built with cargo first (see
// README.md). JAVA_HOME and the workspace root are read from the
// environment so the build works on any machine.
val javaHome: String = System.getenv("JAVA_HOME")
    ?: throw GradleException("JAVA_HOME is not set")
val workspaceRoot: String = System.getenv("CODEC_WORKSPACE")
    ?: projectDir.parentFile.parentFile.absolutePath

tasks.register<Exec>("buildJniLibrary") {
    val outDir = layout.buildDirectory.dir("jni").get().asFile
    doFirst { outDir.mkdirs() }
    val include = "$workspaceRoot/crates/codec-whatttttttttttt-c/include"
    val staticLib =
        "$workspaceRoot/target/debug/libcodec_whatttttttttttt.a"
    commandLine(
        "g++", "-shared", "-fPIC", "-O2",
        "-I$javaHome/include", "-I$javaHome/include/linux",
        "-I$include",
        "src/main/cpp/jni_bridge.cpp",
        staticLib, "-lm", "-lpthread", "-ldl",
        "-o", "$outDir/libcodecwhat_jni.so"
    )
}

tasks.register<JavaExec>("runTests") {
    dependsOn("buildJniLibrary", "testClasses")
    classpath = sourceSets["test"].runtimeClasspath
    main = "dev.codecwhat.DecoderTestKt"
    systemProperty("java.library.path", layout.buildDirectory.dir("jni").get().asFile.absolutePath)
}

tasks.named("check") {
    dependsOn("runTests")
}
