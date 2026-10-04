# Microsoft Visual C++ runtime — redistribution scope

Reviewed: 2026-10-04. Microsoft Visual C++ runtime DLLs are proprietary Microsoft
components, **not covered by Neo's Apache-2.0 license**. This document does not
assert that Neo, its contributors, or its distributor bought a Visual Studio
license, accepted a particular agreement, or received redistribution approval.

## Applicable product, not just a DLL name

The local evidence examined for this review is Visual Studio **Build Tools 2026**,
product `Microsoft.VisualStudio.Product.BuildTools`, catalog build
`18.8.12023.21` (display `18.8.2`), x64 Redist directory
`VC/Redist/MSVC/14.51.36231/x64/Microsoft.VC145.CRT`.
This identifies a local candidate source, **not the contents or entitlement of a
GitHub Actions release runner**. Installer metadata did not affirm a complete
installation. A release must record its own exact product, source and DLL hashes;
`windows-latest`, a directory name, and successful compilation are not licenses.

Official sources:

- [Visual Studio license directory](https://visualstudio.microsoft.com/license-terms/).
- [Diagnostic and Build Tools 2026 terms](https://visualstudio.microsoft.com/license-terms/vs2026-ga-diagnostic-buildtools/).
- [Enterprise / Professional / Trial 2026 terms](https://visualstudio.microsoft.com/license-terms/vs2026-ga-pro-enterprise/).
- [Visual C++ V14 Redistributable and Runtime 2026 terms](https://visualstudio.microsoft.com/license-terms/vs2026-ga-visualcpp-v14-redist-runtime/).
- [2026 REDIST list](https://learn.microsoft.com/en-us/visualstudio/releases/2026/redistribution),
  also referenced by Microsoft as <https://aka.ms/vs/18/redistribution>.
- [Microsoft deployment guidance](https://learn.microsoft.com/en-us/cpp/windows/redistributing-visual-cpp-files?view=msvc-170).

The 2026 REDIST list explicitly covers Visual Studio Enterprise, Professional,
Community and Build Tools, **conditional on a validly licensed copy and its
applicable terms**. It includes unmodified files under `VC/redist`, with stated
exclusions including debug/non-redistributable files; preview components are not
licensed for redistribution. This is a list of eligible components, not an
unconditional grant to anyone who obtains those bytes.

Build Tools 2026 generally requires a valid Visual Studio product license. Its
separate exception for building qualifying third-party open-source C++ dependencies
must not be expanded into a license for all development or CRT redistribution.
The Runtime EULA's permission to install and use copies likewise is not a grant
to redistribute them. Do not substitute Community terms for Build Tools or
Enterprise, or substitute 2022 terms for an observed 2026 product.

## Distributor obligations and boundaries

The distributor must establish the applicable license/contract and redistribution
rights for its actual build and packaging arrangement. For example, the 2026
Enterprise/Professional full-use EULA's Distributable Code section requires
significant primary application functionality, protective terms agreed to by
external end users and distributors, and the specified indemnity. It restricts
previews, misleading use of Microsoft trademarks and subjecting Microsoft code
to an Excluded License. **This summary does not itself implement those obligations
or establish that this full-use EULA governs a hosted runner.** Subscription or
service agreements may supply different applicable terms.

GitHub-hosted access to installed Visual Studio is not, by itself, proof of Neo's
right to redistribute that installation's CRT. Neo's release owner must obtain
and retain the applicable basis, with its organization's licensing/legal owner
where applicable. If relying solely on hosted-runner rights, unresolved coverage
must be clarified with GitHub Support and, as needed, Microsoft licensing support.
No such clarification or approval is asserted here.

Only the release's required, allowlisted, unmodified x64 CRT DLLs should be placed
next to `neo.exe`, with byte-for-byte source evidence. Do not source DLLs from
`System32`, `SysWOW64`, `WinSxS`, or debug folders. Neo's checker additionally
excludes OneCore. Local app-folder deployment requires the distributor to track
security updates and ship updated application packages; it does not acquire the
central Redistributable installer's automatic servicing merely by copying DLLs.

The independent `tools/check_redist_evidence.py` can record and recheck source
paths, exact VS metadata, fixed PE file/product versions, SHA-256 values and a
local license-policy reference. A successful check means evidence consistency,
**not legal authorization, digital-signature authentication, complete dependency
coverage, or end-user agreement acceptance**. It does not modify the existing
release workflow and cannot by itself prevent that workflow from publishing.

## Runtime EULA source and reading copy

Official document:
<https://visualstudio.microsoft.com/wp-content/uploads/2025/10/Visual-C-V14-License-Redistributable_and_Runtime_ENU.docx>

Retrieved 2026-10-04; 39,553 bytes; SHA-256:
`08651651a7602fc7c0e2763de0fde1ff9f868df2780597cd1775ee9d6441c783`.
Title: **MICROSOFT VISUAL C++ V14 REDISTRIBUTABLE and RUNTIME**;
last updated October 1, 2025; EULA ID `Cpp_v14_ENU.1033`.

The following reading copy is extracted from the official DOCX's Word body
paragraphs, in order, without translating the text. Word layout and generated
list numbering are not reproduced; the linked DOCX is authoritative. It is
included for the **runtime installation/use scope**, not as Neo's redistribution
grant, an assertion of agreement acceptance, or a replacement for the
applicable Visual Studio/hosted-service terms. Retaining the text here alone does
not establish that end users agreed to all required protective terms.

<!-- BEGIN OFFICIAL DOCX READING COPY -->

MICROSOFT SOFTWARE LICENSE TERMS

MICROSOFT VISUAL C++ V14 REDISTRIBUTABLE and RUNTIME 

Last Updated: October 1, 2025

These license terms are an agreement between Microsoft Corporation (or based on where you live, one of its affiliates) and you. They apply to the software named above. The terms also apply to any Microsoft services or updates for the software, except to the extent those have different terms.

Unless otherwise stated, these license terms remain in effect for this version of the software and any updates or revisions released. Microsoft may update or modify these license terms at any time. It is your responsibility to regularly review the license terms and any related announcements. The most current version of the license terms will always be available at https://aka.ms/VisualStudioLicenses. By continuing to use the software after changes are posted, you agree to the updated terms.

BY USING THE SOFTWARE, YOU ACCEPT THESE TERMS. IF YOU DO NOT ACCEPT THEM, DO NOT USE THE SOFTWARE.

IF YOU COMPLY WITH THESE LICENSE TERMS, YOU HAVE THE RIGHTS BELOW.

INSTALLATION AND USE RIGHTS. 

You may install and use any number of copies of the software.

FEEDBACK. If you give feedback, suggestions, or recommendations (collectively, “Feedback”) about the software to Microsoft, you give to Microsoft, without charge, the right to use, share, and commercialize your Feedback in any way and for any purpose. You will not give Feedback that is a third party’s confidential information or subject to a license that requires Microsoft to license its software or documentation to third parties because we include your Feedback in them. These rights survive this agreement.

TERMS FOR SPECIFIC COMPONENTS.

Third Party or Open Source Components. The software may include third party or open source components with separate legal notices or governed by other agreements, as may be described in the notices file(s) accompanying the software. 

SCOPE OF LICENSE. The software is licensed, not sold. These license terms only give you some rights to use the software. Microsoft reserves all other rights. Unless applicable law gives you more rights despite this limitation, you may use the software only as expressly permitted in these license terms. In doing so, you must comply with any technical limitations in the software that only allow you to use it in certain ways. For example, if Microsoft technically limits or disables extensibility for the software, you may not extend the software by, among other things, loading or injecting into the software any non-Microsoft add-ins, macros, or packages; modifying the software registry settings; or adding features or functionality equivalent to that found in Microsoft products and services. You may not

work around any technical limitations in the software;

reverse engineer, decompile or disassemble the software, or otherwise attempt to derive the source code for the software, except and only to the extent required by third party licensing terms governing use of certain open source components that may be included in the software;

remove, minimize, block or modify any notices of Microsoft or its suppliers in the software; 

use the software in any way that is against the law; 

share, publish, rent, lease, or otherwise distribute the software or any of its code; or 

provide the software as a stand-alone offering or combined with any of your applications for others to use, or transfer the software or these license terms to any third party.

COMPLIANCE WITH TRADE LAWS. Microsoft products, software, technology, and services (“Items”) may be subject to U.S. and other countries’ export jurisdictions. Each party will comply with all laws and regulations applicable to the import or export of the Items, including, without limitation, trade laws of the U.S., EU, and UK, such as the U.S. Export Administration Regulations, sanctions regulations administered by the U.S. Office of Foreign Assets Control, the EU Dual Use Regulation 2021/821, and/or other end-user, end use, and destination restrictions (“Trade Laws”) as well as the global legal compliance standards detailed in the Microsoft Standards of Business Conduct. You will not, and will ensure your Affiliates will not, take any action that causes Microsoft to violate applicable Trade Laws. Microsoft may suspend or terminate this agreement immediately without notice to the extent that Microsoft reasonably believes that performance would cause it to violate Trade Laws or put it at risk of becoming subject to sanctions and penalties under such laws. You remain responsible for its and for your Affiliates’ compliance with this section. For additional information, see www.microsoft.com/exporting.

You must ensure that you, any of your directors or officers, or to the best of your knowledge, your employees or agents, Affiliates, or Affiliates’ directors, officers, employees, or agents are not sanctioned persons designated under applicable Trade Laws.

You shall provide Microsoft with prompt written notice if you or any of your Affiliates receive notice from any government agency of any investigation, inquiry, or proceeding for any actual or alleged noncompliance of or with any Trade Laws related to activities subject to this Agreement.

You shall comply with global use restrictions in Microsoft’s product or service terms relevant to this agreement, as updated from time to time.

You shall not make available Microsoft Items to any entity or individual designated on any export or sanctions restriction lists under Trade Laws, in each case, without specific government authorization required by Trade Laws.

For the purposes of this Section 8, “Affiliate” means any legal entity that directly or indirectly owns, is owned by, or is commonly owned with the applicable party. “Own” means having more than 50% ownership or the right to direct the management of the entity.

SUPPORT. Because the software is “as is,” we may not provide support services for it.

ENTIRE AGREEMENT. These license terms, and the terms for supplements, updates, Internet-based services and support services, are the entire agreement for the software and support services.

APPLICABLE LAW. If you acquired the software in the United States, Washington state law applies to interpretation of and claims for breach of these license terms, and the laws of the state where you live apply to all other claims. If you acquired the software in any other country, its laws apply.

CONSUMER RIGHTS; REGIONAL VARIATIONS. These license terms describe certain legal rights. You may have other rights, including consumer rights, under the laws of your state or country. Separate and apart from your relationship with Microsoft, you may also have rights with respect to the party from which you acquired the software. These license terms do not change those other rights if the laws of your state or country do not permit it to do so. For example, if you acquired the software in one of the below regions, or if mandatory country law applies, then the following provisions apply to you:

Australia. You have statutory guarantees under the Australian Consumer Law and nothing in these license terms is intended to affect those rights.

Canada. You may stop receiving updates on your device by turning off Internet access. If and when you re-connect to the Internet, the software will resume checking for and installing updates. The product documentation, if any, may also specify how to turn off updates for your specific device or software.

Germany and Austria.

Warranty. The properly licensed software will perform substantially as described in any Microsoft materials that accompany the software. However, Microsoft gives no contractual guarantee in relation to the licensed software.

Limitation of Liability. In case of intentional conduct, gross negligence, claims based on the Product Liability Act, as well as, in case of death or personal or physical injury, Microsoft is liable according to the statutory law.

Subject to the foregoing clause (ii), Microsoft will only be liable for slight negligence if Microsoft is in breach of such material contractual obligations, the fulfillment of which facilitate the due performance of these license terms, the breach of which would endanger the purpose of these license terms and the compliance with which a party may constantly trust in (so-called "cardinal obligations"). In other cases of slight negligence, Microsoft will not be liable for slight negligence.

DISCLAIMER OF WARRANTY. The software is licensed “as-is.” You bear the risk of using it. Microsoft gives no express warranties, guarantees or conditions. To the extent permitted under your local laws, Microsoft excludes the implied warranties of merchantability, fitness for a particular purpose and non-infringement.

LIMITATION ON DAMAGES. You can recover from Microsoft and its suppliers only direct damages up to U.S. $5.00. You cannot recover any other damages, including consequential, lost profits, special, indirect or incidental damages.

This limitation applies to (a) anything related to the software, services, content (including code) on third party Internet sites, or third party applications; and (b) claims for breach of contract, breach of warranty, guarantee or condition, strict liability, negligence, or other tort to the extent permitted by applicable law.

It also applies even if Microsoft knew or should have known about the possibility of the damages. The above limitation or exclusion may not apply to you because your country may not allow the exclusion or limitation of incidental, consequential or other damages.

Please note: As this software is distributed in Canada, some of the clauses in this agreement are provided below in French. 

Remarque: Ce logiciel étant distribué au Canada, certaines des clauses dans ce contrat sont fournies ci-dessous en français. 

EXONÉRATION DE GARANTIE. Le logiciel visé par une licence est offert « tel quel ». Toute utilisation de ce logiciel est à votre seule risque et péril. Microsoft n’accorde aucune autre garantie expresse. Vous pouvez bénéficier de droits additionnels en vertu du droit local sur la protection des consommateurs, que ce contrat ne peut modifier. La ou elles sont permises par le droit locale, les garanties implicites de qualité marchande, d’adéquation à un usage particulier et d’absence de contrefaçon sont exclues. 

LIMITATION DES DOMMAGES-INTÉRÊTS ET EXCLUSION DE RESPONSABILITÉ POUR LES DOMMAGES. Vous pouvez obtenir de Microsoft et de ses fournisseurs une indemnisation en cas de dommages directs uniquement à hauteur de 5,00 $ US. Vous ne pouvez prétendre à aucune indemnisation pour les autres dommages, y compris les dommages spéciaux, indirects ou accessoires et pertes de bénéfices. 

Cette limitation concerne: 

tout ce qui est relié au logiciel, aux services ou au contenu (y compris le code) figurant sur des sites Internet tiers ou dans des programmes tiers; et 

les réclamations au titre de violation de contrat ou de garantie, ou au titre de responsabilité stricte, de négligence ou d’une autre faute dans la limite autorisée par la loi en vigueur. 

Elle s’applique également, même si Microsoft connaissait ou devrait connaître l’éventualité d’un tel dommage. Si votre pays n’autorise pas l’exclusion ou la limitation de responsabilité pour les dommages indirects, accessoires ou de quelque nature que ce soit, il se peut que la limitation ou l’exclusion ci-dessus ne s’appliquera pas à votre égard. 

EFFET JURIDIQUE. Le présent contrat décrit certains droits juridiques. Vous pourriez avoir d’autres droits prévus par les lois de votre pays. Le présent contrat ne modifie pas les droits que vous confèrent les lois de votre pays si celles-ci ne le permettent pas.

EULA ID: Cpp_v14_ENU.1033

<!-- END OFFICIAL DOCX READING COPY -->

## External-prerequisite release policy (2026-10-04 addendum)

The current release policy is **external-prerequisite**, not app-local CRT
redistribution. End users obtain and install the official Microsoft Visual C++
v14 **x64** Redistributable independently under Microsoft's terms:
<https://aka.ms/vc14/vc_redist.x64.exe>. Neo does not bundle the installer or CRT
DLLs, download them automatically, install them, or request elevation for them.
Microsoft's separate installation may require an administrator.

For the currently observed 14.51 build toolset, the conservative release
prerequisite is **14.51.36247.0 or newer**; use the latest supported official
release. This is a chosen prerequisite, not an exact minimum established by PE
import names or the linker version. The observed compiler/source directory
14.51.36231 is not evidence that an older runtime satisfies this package. A
runtime must be at least as new as the build tools; newer build inputs require
this floor to be reviewed again. An older CI toolset (such as 14.44) does not
lower this release-wide prerequisite. The Windows platform floor is unchanged.

`tools/check_release.py --crt-policy external` is the CLI default. It never
copies CRT DLLs and rejects allowlisted CRT names anywhere in the package,
including unused root DLLs and nested runtime directories. It reports required
CRT names from direct/delay imports of `neo.exe` and `runtime/onnx/*.dll`, while
retaining their x64 PE/import checks and the known eSpeak/Piper export blocker.
It does not inspect the user's external CRT, its transitive dependencies,
complete in-process imports, or actual runtime compatibility. A successful
static check is **not** a clean native-link, GPL-free, or legal clearance report.

After welcome/license and before installation file mutations, the Neo installer
reads `Installed` (DWORD) and `Version` (string) at
`HKLM\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64` in the **64-bit registry
view**. Missing, malformed, inaccessible or too-old registration aborts the
installation without replacing old files and displays the official URL. This
is only a registry prerequisite check: registration is not proof of actual DLL
versions, integrity, exported symbols, or successful loading. No credentials
are read. Portable users must satisfy the prerequisite themselves.

The Python `check_package(..., crt_policy="app-local")` default remains for
existing callers. The CLI requires explicit `--crt-policy app-local` when
`--redist-dir` is supplied; it does not silently reinterpret or ignore the
source. The existing recursive app-local PE verification and separate
redistribution-evidence checks remain available for a future **approved**
bundling arrangement. None of the preceding document or EULA is superseded or
removed; its app-local obligations still apply if that mode is used.

External installation avoids Neo distributing Microsoft's runtime bytes; it
**does not establish lawful use of development/build tools, satisfy unresolved
GPL/native-link obligations, or grant redistribution approval**. No policy is
changed to `APPROVED`, and independent legal/release blockers remain in force.
