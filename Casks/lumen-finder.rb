# Homebrew cask for Lumen. This repository doubles as its tap:
#
#   brew tap lyinet/lumen-finder https://github.com/lyinet/lumen-finder
#   brew install --cask lumen-finder
#
# CI rewrites version and sha256 after every tagged release.
cask "lumen-finder" do
  version "0.9.0"
  sha256 "c7b2829726c0b3f66cc90952b4607052a8f6659baeb777ca8571d2e2c10ae426"

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
    "~/Library/Logs/app.lumen.desktop",
    "~/Library/Preferences/app.lumen.desktop.plist",
    "~/Library/WebKit/app.lumen.desktop",
  ]
end
