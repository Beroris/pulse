# PawnIO modules

Two modules from [PawnIO.Modules](https://github.com/namazso/PawnIO.Modules), release
[0.2.11](https://github.com/namazso/PawnIO.Modules/releases/tag/0.2.11). They're the official
signed builds, unchanged, and are built into the exe. The PawnIO driver runs only signed modules.

| File | What it allows | SHA-256 |
|---|---|---|
| `AMDFamily17.bin` | Reading a fixed list of AMD CPU registers, including the temperature (SMN `0x59800`) and the package energy counter. AMD family 17h to 1Ah only. | `DAE74615761B78BDF064DFB3E136252DDCC6FC727D88F14738D0E5800D427A91` |
| `IntelMSR.bin` | Reading a fixed list of Intel CPU registers, including the thermal status and energy counters. Intel only. | `D6ED85D65AB17A22F813EF98207D6D537155EE2DED5976A21CB48413C9B92E5F` |

Pulse only uses them to read. It never calls the few write functions they have.

The modules are licensed under the GNU Lesser General Public License 2.1 (`PawnIO.Modules-LICENSE.txt`).
Their source is the `.p` files of the same name in the PawnIO.Modules repository at that tag.

The PawnIO driver itself isn't included. Pulse uses it only if it is already
installed, for example by another hardware tool, or from [pawnio.eu](https://pawnio.eu).
