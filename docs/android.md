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

## Messages while the app is in the background

Messages arrive over a connection the Rust engine holds open, and Android stops
processes it considers idle. The app therefore runs a **foreground service**
(`ConnectionService`), which is Android's way of saying the process is doing
something for the user; the price it charges is a notification the user can
see, sitting quietly in the shade. Swiping the app away stops the service, so
the notification never outlives the user's intent.

The alternative was push through Firebase, which would mean a Google project
and telling Google who to wake and when. Message contents would still be
encrypted, but *who is messaging whom* is exactly the metadata this project
avoids handing anyone, so the notification is the cheaper price.

The notification about an incoming message is raised from Rust rather than from
the interface: the webview is frozen while the app is off screen, so by the
time it could react the message would be old news.

Android 13 and later also need `POST_NOTIFICATIONS` granted at runtime; the app
asks on first start.
