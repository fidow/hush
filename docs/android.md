# Building the Android client

The Android app is the same app: the same Rust core (identity, PQXDH sessions,
Double Ratchet, local database) and the same TypeScript interface, packaged by
Tauri for Android. Only the layout adapts — on a narrow screen the contact list
and the conversation take turns instead of sharing the window — and the two
desktop-only settings (tray, self-update) are hidden.

## What the machine needs

- **JDK 17 or newer.** `JAVA_HOME` must point at it, not at an old JRE.
- **Android SDK** with platform 34, build-tools 34 and platform-tools.
- **Android NDK** (tested with 27.1.12297006), needed to compile Rust and
  libsignal for `aarch64-linux-android`.
- Rust targets: `rustup target add aarch64-linux-android` (plus
  `armv7-linux-androideabi`, `i686-linux-android`, `x86_64-linux-android` for
  other ABIs).

Without Android Studio, the SDK installs from Google's command-line tools:

```powershell
$sdk = "$env:LOCALAPPDATA\Android\Sdk"
# unzip commandlinetools-win-*.zip into $sdk\cmdline-tools\latest
& "$sdk\cmdline-tools\latest\bin\sdkmanager.bat" --licenses
& "$sdk\cmdline-tools\latest\bin\sdkmanager.bat" `
    "platform-tools" "platforms;android-34" "build-tools;34.0.0" "ndk;27.1.12297006"
```

## Building

```powershell
$env:ANDROID_HOME = "$env:LOCALAPPDATA\Android\Sdk"
$env:NDK_HOME     = "$env:LOCALAPPDATA\Android\Sdk\ndk\27.1.12297006"
$env:JAVA_HOME    = "C:\Program Files\Java\jdk-21"
cd apps/desktop
npm run tauri android build -- --apk --target aarch64
```

**On Windows this fails at the last step** with "Creation symbolic link is not
allowed for this system": Tauri symlinks the compiled `.so` into the Gradle
project, which Windows only allows in developer mode. Either turn that on, or
copy the library and call Gradle yourself, which is what the build above does
everywhere else anyway:

```powershell
copy target\aarch64-linux-android\release\libhush_desktop_lib.so `
     apps\desktop\src-tauri\gen\android\app\src\main\jniLibs\arm64-v8a\
cd apps\desktop\src-tauri\gen\android
.\gradlew.bat assembleRelease
```

The APK lands in
`apps/desktop/src-tauri/gen/android/app/build/outputs/apk/release/`.

## Signing

Android refuses to install an unsigned release APK. The signing key is read
from `~/.android/hush-keystore.properties` (or wherever
`HUSH_ANDROID_KEYSTORE` points), which holds:

```properties
password=…
keyAlias=hush
storeFile=C:/Users/you/.android/hush-release.jks
```

Create one with:

```powershell
keytool -genkeypair -v -keystore "$env:USERPROFILE\.android\hush-release.jks" `
        -alias hush -keyalg RSA -keysize 4096 -validity 10000
```

Keep that file and its password: Android identifies an app by its signing key,
so **losing it means users cannot update — only uninstall and reinstall**. It
is deliberately outside the repository, and `.gitignore` refuses `*.jks` in
case it is ever copied in.

## What is not there yet

**Messages arrive while the app is open.** The stream is an HTTP connection the
system suspends once the app goes to the background, so a message sent then
shows up when the app is opened again, not as a notification. Fixing that means
either push through Firebase — a Google project, and the server learning who to
wake and when — or a foreground service, which shows a permanent notification.
That is a design decision, not a missing line of code.
