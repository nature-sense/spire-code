|   GPIO | Signal Name | Connected To                                                | Remarks                                                      |
| -----: | ----------- | ----------------------------------------------------------- | ------------------------------------------------------------ |
|  GPIO0 | GPIO0       | XTAL_32K_N / (reserved pad / pin header)                    | To use as normal GPIO, consider removing R32 to disable XTAL_32K function |
|  GPIO1 | GPIO1       | XTAL_32K_P / (reserved pad / pin header)                    | To use as normal GPIO, consider removing R36 to disable XTAL_32K function |
|  GPIO2 | GPIO2       | Reserved pad / pin header                                   | Expansion header GPIO2                                       |
|  GPIO3 | GPIO3       | Reserved pad / pin header                                   | Expansion header GPIO3                                       |
|  GPIO4 | GPIO4       | Reserved pad / pin header                                   | Expansion header GPIO4                                       |
|  GPIO5 | GPIO5       | Reserved pad / pin header                                   | Expansion header GPIO5                                       |
|  GPIO6 | GPIO        | Expansion header / ESP32-C5-MINI-1U                         | C5 IO02                                                      |
|  GPIO7 | I2C SDA     | ES8311, GT911, LCD backlight, camera SCCB, expansion header | Shared I2C SDA                                               |
|  GPIO8 | I2C SCL     | ES8311, GT911, LCD backlight, camera SCCB, expansion header | Shared I2C SCL                                               |
|  GPIO9 | I2S DOUT    | ES8311 DSDIN                                                | Audio playback data                                          |
| GPIO10 | I2S WS      | ES8311 LRCK                                                 | Audio frame sync                                             |
| GPIO11 | I2S DIN     | ES8311 ASDOUT                                               | Audio capture data                                           |
| GPIO12 | I2S SCLK    | ES8311 SCLK                                                 | Audio bit clock                                              |
| GPIO13 | I2S MCLK    | ES8311 MCLK                                                 | Audio master clock                                           |
| GPIO14 | GPIO14      | ESP32-C5-MINI-1U                                            | C5 SDIO D0                                                   |
| GPIO15 | GPIO15      | ESP32-C5-MINI-1U                                            | C5 SDIO D1                                                   |
| GPIO16 | GPIO16      | ESP32-C5-MINI-1U                                            | C5 SDIO D2                                                   |
| GPIO17 | GPIO17      | ESP32-C5-MINI-1U                                            | C5 SDIO D3                                                   |
| GPIO18 | GPIO18      | ESP32-C5-MINI-1U                                            | C5 SDIO CLK                                                  |
| GPIO19 | GPIO19      | ESP32-C5-MINI-1U                                            | C5 SDIO CMD                                                  |
| GPIO20 | GPIO20      | Reserved pad / pin header                                   | Expansion header GPIO20                                      |
| GPIO21 | GPIO21      | Reserved pad / pin header                                   | Expansion header GPIO21                                      |
| GPIO22 | GPIO22      | Reserved pad / pin header                                   | Expansion header GPIO22                                      |
| GPIO23 | GPIO23      | Reserved pad / pin header                                   | Expansion header GPIO23                                      |
| GPIO24 | GPIO24      | Reserved pad / pin header                                   | Expansion header GPIO24 / expansion header USB1P1_N0         |
| GPIO25 | GPIO25      | Reserved pad / pin header                                   | Expansion header GPIO25 / expansion header USB1P1_P0         |
| GPIO26 | GPIO26      | Reserved pad / pin header                                   | Expansion header GPIO26 / expansion header USB1P1_N1         |
| GPIO27 | GPIO27      | Reserved pad / pin header                                   | Expansion header GPIO27 / expansion header USB1P1_P1         |
| GPIO28 | GPIO28      | IP101GRI RMII CRS_DV                                        | Carrier sense / receive data valid multiplexed signal, PHY → ESP32-P4 (input) |
| GPIO29 | GPIO29      | IP101GRI RMII RXD0                                          | Receive data bit 0, PHY → ESP32-P4 (input)                   |
| GPIO30 | GPIO30      | IP101GRI RMII RXD1                                          | Receive data bit 1, PHY → ESP32-P4 (input)                   |
| GPIO31 | GPIO31      | IP101GRI SMI MDC                                            | PHY management interface clock, ESP32-P4 → PHY (output)      |
| GPIO32 | GPIO32      | Reserved pad / pin header                                   | Expansion header GPIO32                                      |
| GPIO33 | GPIO33      | Reserved pad / pin header                                   | Expansion header GPIO33                                      |
| GPIO34 | GPIO34      | IP101GRI RMII TXD0                                          | Transmit data bit 0, ESP32-P4 → PHY (output)                 |
| GPIO35 | GPIO35      | IP101GRI RMII TXD1 / BOOT                                   | Strapping pin; transmit data bit 1, ESP32-P4 → PHY (output)  |
| GPIO36 | GPIO36      | Reserved pad / pin header                                   | Strapping pin; external 3.3 V pull-up                        |
| GPIO37 | UART0_TXD   | Expansion header / serial interface                         | UART0 TX; not recommended for general-purpose use            |
| GPIO38 | UART0_RXD   | Expansion header / serial interface                         | UART0 RX; not recommended for general-purpose use            |
| GPIO39 | SD_D0       | TF card                                                     | SDMMC 4-bit data line                                        |
| GPIO40 | SD_D1       | TF card                                                     | SDMMC 4-bit data line                                        |
| GPIO41 | SD_D2       | TF card                                                     | SDMMC 4-bit data line                                        |
| GPIO42 | SD_D3       | TF card                                                     | SDMMC 4-bit data line                                        |
| GPIO43 | SD_CLK      | TF card                                                     | SDMMC clock line                                             |
| GPIO44 | SD_CMD      | TF card                                                     | SDMMC command line                                           |
| GPIO45 | SD_VDD_EN   | TF card power control / expansion header                    | Low to enable; not recommended for general-purpose use while TF function is active |
| GPIO46 | GPIO46      | Reserved pad / pin header                                   | Expansion header GPIO46                                      |
| GPIO47 | GPIO47      | Reserved pad / pin header                                   | Expansion header GPIO47                                      |
| GPIO48 | GPIO48      | Reserved pad / pin header                                   | Expansion header GPIO48                                      |
| GPIO49 | GPIO49      | IP101GRI RMII TX_EN                                         | Transmit enable, ESP32-P4 → PHY (output)                     |
| GPIO50 | GPIO50      | IP101GRI RMII REF_CLK                                       | 50 MHz RMII reference clock, PHY → ESP32-P4 (input)          |
| GPIO51 | GPIO51      | IP101GRI PHY RESET                                          | PHY hardware reset control, ESP32-P4 → PHY (GPIO output)     |
| GPIO52 | GPIO52      | IP101GRI SMI MDIO                                           | PHY management interface data, bidirectional                 |
| GPIO53 | PA_CTRL     | NS4150B enable / expansion header                           | Active high; not recommended for general-purpose use while audio function is active |
| GPIO54 | C5_CHIP_PU  | ESP32C5 enable / expansion header                           | High to enable, low to reset                                 |

| Signal     | GPIO / Signal  | Description                                             |
| ---------- | -------------- | ------------------------------------------------------- |
| `MIPI-DSI` | MIPI interface | 2-lane display interface                                |
| `LCD_RST`  | Not connected  | No dedicated GPIO for LCD reset                         |
| `LCD_BL`   | I2C `0x45`     | Backlight controlled via register `0x96`                |
| `TP_SCL`   | `GPIO8`        | GT911 touch I2C SCL, shared bus                         |
| `TP_SDA`   | `GPIO7`        | GT911 touch I2C SDA, shared bus                         |
| `TP_RST`   | Not connected  | GT911 reset signal not connected                        |
| `TP_INT`   | Not connected  | GT911 interrupt signal not connected; uses polling mode |



| Signal / Peripheral | GPIO / Signal       | Description                          |
| ------------------- | ------------------- | ------------------------------------ |
| `I2S_MCLK`          | `GPIO13`            | ES8311 master clock                  |
| `I2S_SCLK`          | `GPIO12`            | ES8311 bit clock                     |
| `I2S_WS`            | `GPIO10`            | ES8311 frame sync                    |
| `I2S_DOUT`          | `GPIO9`             | ESP32-P4 output to ES8311            |
| `I2S_DIN`           | `GPIO11`            | ES8311 output to ESP32-P4            |
| Amplifier enable    | `GPIO53`            | Active high                          |
| `SD_D0` / `SD_D1`   | `GPIO39` / `GPIO40` | SDMMC data lines                     |
| `SD_D2` / `SD_D3`   | `GPIO41` / `GPIO42` | SDMMC data lines                     |
| `SD_CLK` / `SD_CMD` | `GPIO43` / `GPIO44` | SDMMC clock and command lines        |
| `SD_VDD_EN`         | `GPIO45`            | TF card power control, low to enable |



| Signal         | GPIO     | Direction / Description                                      |
| -------------- | -------- | ------------------------------------------------------------ |
| `RMII_CRS_DV`  | `GPIO28` | IP101GRI → ESP32-P4; carrier sense / receive data valid multiplexed signal |
| `RMII_RXD0`    | `GPIO29` | IP101GRI → ESP32-P4; receive data bit 0                      |
| `RMII_RXD1`    | `GPIO30` | IP101GRI → ESP32-P4; receive data bit 1                      |
| `SMI_MDC`      | `GPIO31` | ESP32-P4 → IP101GRI; PHY management interface clock          |
| `RMII_TXD0`    | `GPIO34` | ESP32-P4 → IP101GRI; transmit data bit 0                     |
| `RMII_TXD1`    | `GPIO35` | ESP32-P4 → IP101GRI; transmit data bit 1                     |
| `RMII_TX_EN`   | `GPIO49` | ESP32-P4 → IP101GRI; transmit enable                         |
| `RMII_REF_CLK` | `GPIO50` | IP101GRI → ESP32-P4; 50 MHz RMII reference clock             |
| `PHY_RESET`    | `GPIO51` | ESP32-P4 → IP101GRI; PHY hardware reset control              |
| `SMI_MDIO`     | `GPIO52` | Bidirectional; PHY management interface data                 |



| Device        | Model / Function       | I2C Address                              | I2C Pins          | Notes                                             |
| ------------- | ---------------------- | ---------------------------------------- | ----------------- | ------------------------------------------------- |
| Audio Codec   | ES8311                 | 7-bit `0x18`; 8-bit write address `0x30` | `GPIO7` / `GPIO8` | Onboard audio capture and playback                |
| Touch         | GT911                  | `0x5D` / `0x14`                          | `GPIO7` / `GPIO8` | Both addresses are tried; RST / INT not connected |
| LCD Backlight | Backlight controller   | `0x45`                                   | `GPIO7` / `GPIO8` | Brightness register is `0x96`                     |
| Camera SCCB   | External camera module | Depends on module                        | `GPIO7` / `GPIO8` | Shares bus with onboard I2C devices               |