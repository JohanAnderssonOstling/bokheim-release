import groovy.json.JsonSlurper
import org.gradle.api.artifacts.component.ModuleComponentIdentifier

plugins {
    id("com.android.application") version "8.11.0"
}

val fdroidBuild = providers.gradleProperty("bokheim.fdroid").orNull == "true"
val fdroidToolchains = JsonSlurper().parse(rootProject.projectDir.resolve("../../../scripts/fdroid/toolchains.json")) as Map<*, *>
val androidNdkVersion = if (fdroidBuild) "29.0.14206865" else "29.0.13599879"
val rustTarget = "aarch64-linux-android"
val rustAbi = "arm64-v8a"
if (fdroidBuild && providers.gradleProperty("bokheim.googleDrive").orNull == "true") {
    throw GradleException("F-Droid builds cannot enable Google Play Services")
}
// Opt in explicitly for development; GitHub release builds leave Drive out.
val googleDriveEnabled = providers.gradleProperty("bokheim.googleDrive").orNull == "true"
val rustFeatures = if (googleDriveEnabled) "android,google-drive" else "android"
val workspaceRoot = rootProject.projectDir.resolve("../../..").canonicalFile
// Keep the JVM verifier ABI aligned with Reqwest's Rust dependency.
val rustlsVerifierVersion = Regex("name = \"rustls-platform-verifier-android\"\\s+version = \"([^\"]+)\"")
    .find(workspaceRoot.resolve("Cargo.lock").readText())?.groupValues?.get(1)
    ?: throw GradleException("Missing rustls-platform-verifier-android in Cargo.lock")
val desktopVersion = Regex("(?m)^version = \"([^\"]+)\"$")
    .find(workspaceRoot.resolve("apps/desktop-gpui/Cargo.toml").readText())?.groupValues?.get(1)
    ?: throw GradleException("Missing desktop Cargo version")
val versionParts = desktopVersion.split('.').map { it.toIntOrNull() }
if (versionParts.size != 3 || versionParts.any { it == null || it < 0 } ||
    versionParts[0]!! > 2099 || versionParts[1]!! > 999 || versionParts[2]!! > 999) {
    throw GradleException("Android releases require a stable MAJOR.MINOR.PATCH version (major <= 2099, minor/patch <= 999)")
}
val androidVersionCode = versionParts[0]!! * 1_000_000 + versionParts[1]!! * 1_000 + versionParts[2]!!
if (androidVersionCode <= 0) throw GradleException("Android release version must be greater than 0.0.0")
// Environment values take precedence in CI. Local Gradle user properties may
// reference private files without copying passwords into the repository.
fun signingSetting(name: String): String? = providers.environmentVariable(name)
    .orElse(providers.gradleProperty(name)).orNull
fun signingPassword(name: String): String? = signingSetting(name)
    ?: signingSetting("${name}_FILE")?.let { file(it).readText().trimEnd('\r', '\n') }
val releaseKeyStore = signingSetting("BOKHEIM_ANDROID_KEYSTORE")
val releaseStorePassword = signingPassword("BOKHEIM_ANDROID_STORE_PASSWORD")
val releaseKeyAlias = signingSetting("BOKHEIM_ANDROID_KEY_ALIAS")
val releaseKeyPassword = signingPassword("BOKHEIM_ANDROID_KEY_PASSWORD") ?: releaseStorePassword
val productionSigning = listOf(releaseKeyStore, releaseStorePassword, releaseKeyAlias, releaseKeyPassword).all { !it.isNullOrEmpty() }
val localSigning = providers.gradleProperty("bokheim.localSigning").orNull == "true"
val cargoTargetDirectory = workspaceRoot.resolve(if (fdroidBuild) "target/fdroid" else "target")
val pdfiumDirectory = workspaceRoot.resolve(if (fdroidBuild) ".pdfium/fdroid/$rustTarget" else ".pdfium/$rustTarget")
val rustDebugLibrary = cargoTargetDirectory.resolve("$rustTarget/debug/libdesktop_gpui.so")
val rustReleaseLibrary = cargoTargetDirectory.resolve("$rustTarget/release/libdesktop_gpui.so")
val stagedRustDebugLibraries = layout.buildDirectory.dir("rustJniLibs/debug")
val stagedRustReleaseLibraries = layout.buildDirectory.dir("rustJniLibs/release")
val speechDirectory = workspaceRoot.resolve(if (fdroidBuild) "target/android-speech-fdroid" else "target/android-speech")
val fdroidOnnxRuntime by configurations.creating { isTransitive = false }
fun configuredNdk(): File {
    val sdk = System.getenv("ANDROID_HOME") ?: System.getenv("ANDROID_SDK_ROOT")
    val ndk = file(providers.gradleProperty("rust.ndkHome").orNull
        ?: (if (fdroidBuild) sdk?.let { "$it/ndk/$androidNdkVersion" } else null)
        ?: System.getenv("ANDROID_NDK_HOME") ?: System.getenv("NDK_HOME")
        ?: sdk?.let { "$it/ndk/$androidNdkVersion" }
        ?: throw GradleException("Set rust.ndkHome, ANDROID_NDK_HOME, NDK_HOME, or ANDROID_HOME"))
    if (fdroidBuild) {
        check(ndk.resolve("source.properties").readLines().any { it.trim() == "Pkg.Revision = $androidNdkVersion" }) {
            "F-Droid builds require Android NDK $androidNdkVersion"
        }
    }
    return ndk.canonicalFile
}
val launcherIconResources = layout.buildDirectory.dir("generated/res/launcherIcon")

val stageLauncherIcon by tasks.registering(Copy::class) {
    from(workspaceRoot.resolve("apps/ui/design-tokens/assets/icon/bokheim-android-foreground-432.png"))
    into(launcherIconResources.map { it.dir("drawable-xxxhdpi") })
    rename { "bokheim_launcher_foreground.png" }
}

tasks.named("preBuild") { dependsOn(stageLauncherIcon) }

val stageFdroidNativeNotices by tasks.registering(Copy::class) {
    dependsOn("preparePdfium", "prepareSpeech")
    from(pdfiumDirectory) { include("LICENSE", "VERSION", "SOURCE.json", "receipt.json"); into("pdfium") }
    from(speechDirectory.resolve("jniLibs/$rustAbi")) { include("LICENSE", "SOURCE.json", "receipt.json"); into("speech") }
    from("rustls-verifier") { include("LICENSE-*", "source.json"); into("rustls-platform-verifier") }
    from(workspaceRoot) { include("LICENSE", "COPYRIGHT"); into("bokheim") }
    into(layout.buildDirectory.dir("fdroidNotices"))
}


android {
    namespace = "se.bokheim.reader.gpui"
    compileSdk = 36
    if (fdroidBuild) buildToolsVersion = fdroidToolchains["build_tools"].toString()
    ndkVersion = androidNdkVersion

    defaultConfig {
        applicationId = "se.bokheim.reader.gpui"
        ndk { abiFilters += rustAbi }
        minSdk = 26
        targetSdk = 36
        versionCode = androidVersionCode
        versionName = desktopVersion
        testInstrumentationRunner = "se.bokheim.reader.gpui.UpdateMetadataInstrumentation"
        buildConfigField("boolean", "PLAY_STORE", "false")
        buildConfigField("boolean", "FDROID", fdroidBuild.toString())
    }

    buildFeatures.buildConfig = true
    if (fdroidBuild) {
        packaging.jniLibs.keepDebugSymbols.addAll(listOf("**/libpdfium.so", "**/libsherpa-onnx-c-api.so", "**/libonnxruntime.so"))
    }

    testBuildType = "release"

    testOptions.unitTests.isIncludeAndroidResources = true

    sourceSets.getByName("main").java.srcDir(if (googleDriveEnabled) "src/googleDrive/java" else "src/noGoogleDrive/java")
    sourceSets.getByName("test").java.srcDir(if (googleDriveEnabled) "src/googleDriveTest/java" else "src/noGoogleDriveTest/java")
    sourceSets.getByName("main").manifest.srcFile("AndroidManifest.xml")
    sourceSets.getByName("main").res.srcDir(launcherIconResources)
    sourceSets.getByName("main").assets.srcDir(speechDirectory.resolve("assets"))
    if (fdroidBuild) {
        sourceSets.getByName("main").assets.srcDir(stageFdroidNativeNotices)
        sourceSets.getByName("debug").manifest.srcFile("src/fdroid/AndroidManifest.xml")
        sourceSets.getByName("release").manifest.srcFile("src/fdroid/AndroidManifest.xml")
    }
    sourceSets.getByName("debug").assets.srcDir(layout.buildDirectory.dir("taxonomyAssets/debug"))
    sourceSets.getByName("release").assets.srcDir(layout.buildDirectory.dir("taxonomyAssets/release"))
    sourceSets.getByName("debug").jniLibs.srcDir(stagedRustDebugLibraries)
    sourceSets.getByName("release").jniLibs.srcDir(stagedRustReleaseLibraries)

    signingConfigs {
        if (productionSigning) {
            create("production") {
                storeFile = file(releaseKeyStore!!)
                storePassword = releaseStorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }
    buildTypes {
        getByName("release") {
            if (fdroidBuild) vcsInfo.include = false
            signingConfig = when {
                fdroidBuild -> null
                localSigning -> signingConfigs.getByName("debug")
                productionSigning -> signingConfigs.getByName("production")
                else -> null
            }
        }
        // Keep the existing debug/release APK tasks and their distribution
        // behavior. Play shares the optimized native build, but has its own
        // Java policy, manifest and bundle output.
        create("play") {
            initWith(getByName("release"))
            matchingFallbacks += "release"
            buildConfigField("boolean", "PLAY_STORE", "true")
        }
    }

    sourceSets.getByName("play").assets.srcDir(layout.buildDirectory.dir("taxonomyAssets/release"))
    sourceSets.getByName("play").jniLibs.srcDir(stagedRustReleaseLibraries)
}

tasks.register("prefetchFdroidDependencies") {
    doLast {
        check(fdroidBuild) { "Dependency prefetch requires -Pbokheim.fdroid=true" }
        // AAPT2 uses a detached configuration created only when its executable
        // is requested, so the configuration loop alone cannot prefetch it.
        check(androidComponents.sdkComponents.aapt2.get().executable.get().asFile.isFile)
        // AGP also creates its lint runtime configuration lazily during tasks.
        configurations.detachedConfiguration(dependencies.create(
            "com.android.tools.lint:lint-gradle:${fdroidToolchains["lint"]}"
        )).resolve()
        rootProject.allprojects.forEach { project ->
            project.configurations.filter { configuration -> configuration.isCanBeResolved &&
                listOf("test", "debug", "play").none { configuration.name.contains(it, ignoreCase = true) } }
                .forEach { configuration ->
                    // Keep project dependency edges for version selection, while
                    // fetching only external artifacts without compiling projects.
                    configuration.incoming.artifactView {
                        componentFilter { it is ModuleComponentIdentifier }
                    }.files.files
                }
        }
    }
}

val requireReleaseSigning by tasks.registering {
    doLast {
        if (fdroidBuild) return@doLast
        if (!productionSigning && !localSigning) {
            throw GradleException("Configure BOKHEIM_ANDROID_KEYSTORE, STORE_PASSWORD, KEY_ALIAS and KEY_PASSWORD (each with BOKHEIM_ANDROID_ prefix). For an explicitly local optimized APK only, use -Pbokheim.localSigning=true.")
        }
        if (!localSigning && productionSigning && !file(releaseKeyStore!!).isFile) {
            throw GradleException("The configured Android release keystore does not exist")
        }
    }
}

val requirePlayBundleSigning by tasks.registering {
    dependsOn(requireReleaseSigning)
    doLast {
        check(!fdroidBuild) { "F-Droid builds produce unsigned APKs, not Play bundles" }
        if (localSigning) {
            throw GradleException("Google Play bundles require a release/upload key. Use assemblePlay with -Pbokheim.localSigning=true only for local device testing.")
        }
    }
}

dependencies {
    if (googleDriveEnabled) implementation("com.google.android.gms:play-services-auth:22.0.0")
    if (fdroidBuild) {
        implementation(project(":rustls-verifier"))
        val pins = JsonSlurper().parse(workspaceRoot.resolve("shared/reader-speech/source.json")) as Map<*, *>
        add(fdroidOnnxRuntime.name, pins["onnxruntime_coordinate"].toString())
    } else {
        implementation("org.rustls:rustls-platform-verifier:$rustlsVerifierVersion")
    }
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.16.1")
    implementation("androidx.media3:media3-exoplayer:1.8.0")
    implementation("androidx.media3:media3-session:1.8.0")
    implementation(platform("org.jetbrains.kotlin:kotlin-bom:1.8.22"))
    implementation("androidx.games:games-activity:4.4.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.core:core:1.16.0")
}

val preparePdfium by tasks.registering(Exec::class) {
    workingDir(workspaceRoot)
    commandLine("python3", "shared/pdfium/prepare.py", "--target", rustTarget)
    if (fdroidBuild) {
        args("--from-source", "--dest", pdfiumDirectory.absolutePath,
            "--jobs", providers.gradleProperty("bokheim.nativeJobs").orNull ?: "2")
        providers.gradleProperty("bokheim.pdfiumSourceWork").orNull?.let { args("--source-work", it) }
        if (gradle.startParameter.isOffline) args("--offline")
    }
}

val prepareSpeech by tasks.registering(Exec::class) {
    workingDir(workspaceRoot)
    commandLine("python3", "shared/reader-speech/prepare_android.py", "--destination", speechDirectory.absolutePath)
    if (fdroidBuild) {
        doFirst {
            args("--from-source", "--onnxruntime-aar", fdroidOnnxRuntime.singleFile.absolutePath,
                "--ndk", configuredNdk().absolutePath,
                "--jobs", providers.gradleProperty("bokheim.nativeJobs").orNull ?: "2")
            providers.gradleProperty("bokheim.speechSourceWork").orNull?.let { args("--source-work", it) }
            if (gradle.startParameter.isOffline) args("--offline")
        }
    }
}


val buildRustDebug by tasks.registering(Exec::class) {
    dependsOn(preparePdfium, prepareSpeech)
    workingDir(workspaceRoot)
    commandLine(
        "cargo",
        "build",
        "-p",
        "desktop-gpui",
        "--lib",
        "--no-default-features",
        "--features",
        rustFeatures,
        "--target",
        rustTarget,
    )
    doFirst {
        val toolchain = configuredNdk().resolve("toolchains/llvm/prebuilt/linux-x86_64/bin")
        val clang = toolchain.resolve("aarch64-linux-android26-clang")
        val clangCpp = toolchain.resolve("aarch64-linux-android26-clang++")
        val archiveTool = toolchain.resolve("llvm-ar")
        if (!clang.isFile || !clangCpp.isFile || !archiveTool.isFile) {
            throw GradleException("Android NDK toolchain is incomplete under ${configuredNdk()}")
        }
        environment("CC_aarch64_linux_android", clang.absolutePath)
        environment("CXX_aarch64_linux_android", clangCpp.absolutePath)
        environment("AR_aarch64_linux_android", archiveTool.absolutePath)
        environment("CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER", clang.absolutePath)
        environment("SHERPA_ONNX_LIB_DIR", speechDirectory.resolve("jniLibs/$rustAbi").absolutePath)
    }
}

val stageRustDebug by tasks.registering(Copy::class) {
    dependsOn(buildRustDebug)
    from(rustDebugLibrary)
    from(pdfiumDirectory.resolve("libpdfium.so"))
    from(speechDirectory.resolve("jniLibs/$rustAbi")) { include("*.so") }
    into(stagedRustDebugLibraries.map { it.dir(rustAbi) })
}

val buildRustRelease by tasks.registering(Exec::class) {
    dependsOn(preparePdfium, prepareSpeech)
    workingDir(workspaceRoot)
    commandLine(
        "cargo",
        "build",
        "-p",
        "desktop-gpui",
        "--lib",
        "--release",
        "--no-default-features",
        "--features",
        rustFeatures,
        "--target",
        rustTarget,
    )
    doFirst {
        val toolchain = configuredNdk().resolve("toolchains/llvm/prebuilt/linux-x86_64/bin")
        val clang = toolchain.resolve("aarch64-linux-android26-clang")
        val clangCpp = toolchain.resolve("aarch64-linux-android26-clang++")
        val archiveTool = toolchain.resolve("llvm-ar")
        if (!clang.isFile || !clangCpp.isFile || !archiveTool.isFile) {
            throw GradleException("Android NDK toolchain is incomplete under ${configuredNdk()}")
        }
        environment("CC_aarch64_linux_android", clang.absolutePath)
        environment("CXX_aarch64_linux_android", clangCpp.absolutePath)
        environment("AR_aarch64_linux_android", archiveTool.absolutePath)
        environment("CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER", clang.absolutePath)
        environment("SHERPA_ONNX_LIB_DIR", speechDirectory.resolve("jniLibs/$rustAbi").absolutePath)
    }
}

val stageRustRelease by tasks.registering(Copy::class) {
    dependsOn(buildRustRelease)
    from(rustReleaseLibrary)
    from(pdfiumDirectory.resolve("libpdfium.so"))
    from(speechDirectory.resolve("jniLibs/$rustAbi")) { include("*.so") }
    into(stagedRustReleaseLibraries.map { it.dir(rustAbi) })
}

val stageTaxonomyDebug by tasks.registering(Copy::class) {
    dependsOn(buildRustDebug)
    from(cargoTargetDirectory.resolve("$rustTarget/debug")) { include("taxonomy.sqlite3", "taxonomy.digest") }
    into(layout.buildDirectory.dir("taxonomyAssets/debug"))
    doLast { check(destinationDir.resolve("taxonomy.sqlite3").isFile) { "Missing bundled taxonomy" } }
}
val stageTaxonomyRelease by tasks.registering(Copy::class) {
    dependsOn(buildRustRelease)
    from(cargoTargetDirectory.resolve("$rustTarget/release")) { include("taxonomy.sqlite3", "taxonomy.digest") }
    into(layout.buildDirectory.dir("taxonomyAssets/release"))
    doLast { check(destinationDir.resolve("taxonomy.sqlite3").isFile) { "Missing bundled taxonomy" } }
}

tasks.configureEach {
    if (fdroidBuild && (name == "preparePdfium" || name == "prepareSpeech")) {
        (this as Exec).environment("SOURCE_DATE_EPOCH", fdroidToolchains["source_date_epoch"].toString())
    }
    if (fdroidBuild && (name == "buildRustDebug" || name == "buildRustRelease")) {
        val rustTask = this as Exec
        rustTask.environment("CARGO_TARGET_DIR", cargoTargetDirectory.absolutePath)
        rustTask.environment("PDFIUM_PREPARED_DIR", pdfiumDirectory.absolutePath)
        rustTask.environment("PDFIUM_REQUIRE_SOURCE", "1")
        rustTask.environment("CARGO_BUILD_JOBS", providers.gradleProperty("bokheim.rustJobs").orNull ?: "2")
        rustTask.environment("RUSTUP_TOOLCHAIN", fdroidToolchains["rust"].toString())
        rustTask.environment("SOURCE_DATE_EPOCH", fdroidToolchains["source_date_epoch"].toString())
        val userHome = System.getenv("HOME") ?: System.getProperty("user.home")
        val cargoHome = System.getenv("CARGO_HOME") ?: "$userHome/.cargo"
        val rustupHome = System.getenv("RUSTUP_HOME") ?: "$userHome/.rustup"
        rustTask.environment("RUSTFLAGS", listOf(
            "--remap-path-prefix=${workspaceRoot.parentFile}=/build",
            "--remap-path-prefix=$cargoHome=/build/cargo",
            "--remap-path-prefix=$rustupHome=/build/rustup",
        ).joinToString(" "))
        rustTask.args("--locked")
        if (gradle.startParameter.isOffline) rustTask.args("--offline")
    }
    if (fdroidBuild && name.startsWith("merge") && name.endsWith("Assets")) dependsOn(stageFdroidNativeNotices)
    if (name.contains("Lint") || name.startsWith("lint")) {
        if (fdroidBuild) dependsOn(stageFdroidNativeNotices)
        if (name.contains("Debug")) dependsOn(stageTaxonomyDebug)
        if (name.contains("Release") || name.contains("Play")) dependsOn(stageTaxonomyRelease)
    }
    if (name == "mergeDebugAssets") dependsOn(stageTaxonomyDebug)
    if (name == "mergeReleaseAssets" || name == "mergePlayAssets") dependsOn(stageTaxonomyRelease)
    if (name == "mergeDebugAssets" || name == "mergeReleaseAssets" || name == "mergePlayAssets") dependsOn(prepareSpeech)
    if (name == "packageRelease" || name == "signReleaseBundle" || name == "packagePlay" || name == "signPlayBundle") dependsOn(requireReleaseSigning)
    if (name == "signPlayBundle") dependsOn(requirePlayBundleSigning)
    if (name == "mergeDebugJniLibFolders" || name == "mergeDebugNativeLibs") {
        dependsOn(stageRustDebug)
    }
    if (name == "mergeReleaseJniLibFolders" || name == "mergeReleaseNativeLibs" || name == "mergePlayJniLibFolders" || name == "mergePlayNativeLibs") {
        dependsOn(stageRustRelease)
    }
}
