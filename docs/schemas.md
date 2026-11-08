# Schemas

## Nucleo --> HUB75E

The firmware drives two HUB75E panels. GPIOE drives the top panel and GPIOD drives the bottom panel.

Each panel's signals pass through two 74HCT245 buffers for 3.3 V to 5 V logic adjustment before reaching its HUB75E connector.

### Top panel

| **Signal** | **STM32 pin** | **74HCT245** | **Buffer input pin (A)** | **Buffer output pin (B)** | **HUB75E pin** |
| ---------- | ------------- | ------------ | ------------------------ | ------------------------- | -------------- |
| R1         | PE7           | 1            | 2                        | 18                        | 1              |
| G1         | PE8           | 1            | 3                        | 17                        | 2              |
| B1         | PE9           | 1            | 4                        | 16                        | 3              |
| A          | PE10          | 1            | 5                        | 15                        | 9              |
| B          | PE11          | 1            | 6                        | 14                        | 10             |
| C          | PE12          | 1            | 7                        | 13                        | 11             |
| D          | PE13          | 1            | 8                        | 12                        | 12             |
| E          | PE14          | 1            | 9                        | 11                        | 8              |
| CLK        | PE15          | 2            | 2                        | 18                        | 13             |
| LAT        | PE5           | 2            | 3                        | 17                        | 14             |
| OE         | PE6           | 2            | 4                        | 16                        | 15             |
| R2         | PE2           | 2            | 5                        | 15                        | 5              |
| G2         | PE3           | 2            | 6                        | 14                        | 6              |
| B2         | PE4           | 2            | 7                        | 13                        | 7              |

### Bottom panel

| **Signal** | **STM32 pin** | **74HCT245** | **Buffer input pin (A)** | **Buffer output pin (B)** | **HUB75E pin** |
| ---------- | ------------- | ------------ | ------------------------ | ------------------------- | -------------- |
| R1         | PD7           | 1            | 2                        | 18                        | 1              |
| G1         | PD8           | 1            | 3                        | 17                        | 2              |
| B1         | PD9           | 1            | 4                        | 16                        | 3              |
| A          | PD10          | 1            | 5                        | 15                        | 9              |
| B          | PD11          | 1            | 6                        | 14                        | 10             |
| C          | PD12          | 1            | 7                        | 13                        | 11             |
| D          | PD13          | 1            | 8                        | 12                        | 12             |
| E          | PD14          | 1            | 9                        | 11                        | 8              |
| CLK        | PD15          | 2            | 2                        | 18                        | 13             |
| LAT        | PD5           | 2            | 3                        | 17                        | 14             |
| OE         | PD6           | 2            | 4                        | 16                        | 15             |
| R2         | PD2           | 2            | 5                        | 15                        | 5              |
| G2         | PD3           | 2            | 6                        | 14                        | 6              |
| B2         | PD4           | 2            | 7                        | 13                        | 7              |

## Panel

This is the reverse-engineered schema of a 128x64 px panel:

```mermaid
flowchart LR
    MCU["Controller<br/>HUB75E interface"]
    BUF["3 x 74HC245KAJ<br/>input/distribution buffers"]
    INV["74HC04D (U4)<br/>generates internal inversions"]

    subgraph COL["Column / pixel drivers"]
        direction TB
        C1["R1 serial chain<br/>8 x FM6124HJ<br/>128 upper-half red columns"]
        C2["G1 serial chain<br/>8 x FM6124HJ<br/>128 upper-half green columns"]
        C3["B1 serial chain<br/>8 x FM6124HJ<br/>128 upper-half blue columns"]
        C4["R2 serial chain<br/>8 x FM6124HJ<br/>128 lower-half red columns"]
        C5["G2 serial chain<br/>8 x FM6124HJ<br/>128 lower-half green columns"]
        C6["B2 serial chain<br/>8 x FM6124HJ<br/>128 lower-half blue columns"]
    end

    subgraph ROW["Row selection"]
        direction TB
        DEC["8 x TC7262G<br/>8 high-side outputs each"]
        OUT["64 row-select lines<br/>32 scan addresses x 2 halves"]
    end

    PIX["128 x 64 LED matrix"]

    MCU -->|"RGB data, CLK, LAT, OE<br/>(buffer channel routing not fully traced)"| BUF
    BUF -->|"R1"| C1
    BUF -->|"G1"| C2
    BUF -->|"B1"| C3
    BUF -->|"R2"| C4
    BUF -->|"G2"| C5
    BUF -->|"B2"| C6
    C1 --> PIX
    C2 --> PIX
    C3 --> PIX
    C4 --> PIX
    C5 --> PIX
    C6 --> PIX

    MCU -->|"A, B, C"| BUF
    BUF -->|"A -> A0, B -> A1, C -> A2<br/>to all eight TC7262G"| DEC
    MCU -->|"D, E"| BUF
    BUF -->|"D/E and complementary group selects"| DEC
    BUF --> INV
    INV -->|"inverted internal enable signals"| DEC
    DEC --> OUT
    OUT --> PIX
```
