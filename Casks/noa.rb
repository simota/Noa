cask "noa" do
  version "0.1.4"
  sha256 :no_check

  url "https://github.com/simota/Noa/releases/download/v#{version}/Noa-#{version}-macos-arm64.zip"
  name "Noa"
  desc "GPU-accelerated terminal emulator written in Rust"
  homepage "https://github.com/simota/Noa"

  depends_on arch: :arm64
  depends_on macos: :ventura

  app "Noa.app"

  uninstall quit: "com.simota.noa"

  zap trash: [
    "~/Library/Application Support/noa",
    "~/Library/Saved Application State/com.simota.noa.savedState",
  ]
end
