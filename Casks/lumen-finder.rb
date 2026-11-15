# Homebrew cask for Lumen. This repository doubles as its tap:
#
#   brew tap lyinet/lumen-finder https://github.com/lyinet/lumen-finder
#   brew install --cask lumen-finder
#
# CI rewrites version and sha256 after every tagged release.
cask "lumen-finder" do
  version "0.6.0"
  sha256 "a1acd61c24ce4773b0b7a37990256b6d45454f160423c29735bee3302c21b5b9"

  url "https://github.com/lyinet/lumen-finder/releases/download/v#{version}/Lumen_#{version}_universal.dmg"
  name "Lumen"
  desc "Find any personal file by name, pinyin, contents or meaning"
  homepage "https://github.com/lyinet/lumen-finder"

  livecheck do
    url :url
    strategy :github_latest
  end

  # Lumen updates itself in place (Settings → Updates).
  auto_updates true
  depends_on macos: ">= :big_sur"

  app "Lumen.app"
  binary "#{appdir}/Lumen.app/Contents/MacOS/lumen"

  # Until releases are notarized with a Developer ID, clear the download
  # quarantine so Gatekeeper does not block the first launch.
  postflight do
    system_command "/usr/bin/xattr",
                   args:         ["-dr", "com.apple.quarantine", "#{appdir}/Lumen.app"],
                   must_succeed: false
  end

  uninstall quit: "app.lumen.desktop"

  zap trash: [
    "~/Library/Application Support/app.lumen.desktop",
    "~/Library/Caches/app.lumen.desktop",
    "~/Library/LaunchAgents/Lumen.plist",
    "~/Library/Preferences/app.lumen.desktop.plist",
    "~/Library/WebKit/app.lumen.desktop",
  ]
end
