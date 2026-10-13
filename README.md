# OpenNOW-4Handhelds

Handheld port of [OpenNOW](https://github.com/OpenCloudGaming/OpenNOW) for devices such as the R36S (RK3326, dArkOS). The code is derived from that project.

This is not the OpenNOW desktop application. It does not replace OpenNOW on Windows, macOS, or a general Linux desktop.

The client is a console UI (`opennow-handheld`) with the OpenNOW core and streamer beside it. `third_party` holds the Rockchip MPP libraries the R36S decoder needs. `build-r36s.ps1` cross-compiles the port from Windows. The build directory must not contain spaces.

## License

MIT. Copyright (c) 2025 Zortos. See [LICENSE](LICENSE).
