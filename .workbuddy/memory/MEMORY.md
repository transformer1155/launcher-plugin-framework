# NexOS bootloader — 长期项目记忆

## 当前构建/测试入口（用户 2026-11-08 明确，优先于下文所有旧约定）
- 在 **PowerShell** 执行 `vm-run "<命令>"`。`vm-run` = `C:\windows\vm-run.bat`，脚本注明这是 Windows 侧**唯一受支持**的构建/测试通道，不要在宿主直接跑 make/gcc/qemu。
- 原理：SSH 到 VirtualBox VM `Linux`（Ubuntu；NAT 转发 127.0.0.1:2222→guest:22，用户 transform1155，密钥 `%USERPROFILE%\.ssh\id_rsa`），远端仓库 `/media/sf_NexOS/NexOS` = 本机 `E:\NexOS\NexOS` 共享文件夹。VBoxManage 实际在 `C:\Program Files\Oracle\VBoxManage.exe`（**不在** VirtualBox 子目录）。
- 子命令：`status` / `make` / `test` / `test-log` / `clean` / `shell` / `push <本地> <远端>` / `pull <远端> <本地>` / 任意 shell 命令。实测 status 全绿（make/g++/ld/nasm/objcopy/python3/xorriso/mformat/mcopy/qemu 均 OK）；**无 /dev/kvm、svm-flags=0 → QEMU 只能 tcg**。
- **坑**：传入命令里的 `|` `;` `&&` 和嵌套引号会被 cmd.exe 二次解析、在 Windows 侧执行；复杂逻辑先 `vm-run push x.sh /tmp/x.sh` 再 `vm-run "bash /tmp/x.sh"`，或在宿主侧用 PowerShell 过滤输出。
- 并发纪律：CPU 重活走 `tools/verify/with_build_lock.sh`（flock `/tmp/nexos_vm.lock`），开工前 `tools/verify/preflight_idle.sh` 查空闲；vm-run 的 make/test/test-log 已自带同一把锁。
- VM 清单：NexOS、NexOSTest、Windows1.0、Windows 10、Linux；`Linux` 与 `NexOS` 常驻运行；GUI 管理器 `C:\Program Files\Oracle\VirtualBox.exe`。

## 构建产物必须对 VBox VM 可见（用户 2026-11-10 强制）
- `vm-run make` 只更新 `build/os_v2.img`；VirtualBox 的 `NexOS` VM 实际启动 **`build/os_v2_vbox.vdi`**（IDE-0-0）+ `build/data_vbox.vdi`（IDE-1-0），firmware=BIOS，默认 poweroff。**不同步，用户在 VM 里看到的永远是旧镜像**。
- 每次构建后必须执行（Windows 宿主 PowerShell）：`powershell -ExecutionPolicy Bypass -File tools\build_sync.ps1`（= `vm-run make` + `tools\vbox_sync.ps1`）；只同步时 `& 'E:\NexOS\NexOS\tools\vbox_sync.ps1'`。
- `tools/vbox_sync.ps1`：VM 须 poweroff → detach → `convertfromraw` 到临时 VDI → `sethduuid` 保留原 UUID（否则 VBox 报 inaccessible）→ 换文件（旧盘留 `.prev`）→ **无条件 reattach**。已修 PS 5.1 两个坑：VBoxManage 进度写 stderr 会被 `$ErrorActionPreference='Stop'` 当成终止错误；`--machinereadable` 把路径反斜杠转义成 `\\`，直接字符串比较会漏掉 detach/reattach。同步只覆盖系统盘，数据盘保持原样。

## 构建与测试环境（历史约定，2026-08-14 起，已被上节取代）
- 构建统一走 `tools/misc/build_win.sh <target>`（MSYS2 make/nasm + i686/x86_64-elf GCC 13.2.0 @ `C:\Users\trans\elf_tools\bin` + dotnet.ps1 + 托管 python）；测试 `D:\qemu\qemu-system-x86_64.exe`。
- **构建必须用真实 MSYS2 bash 调用**：`C:\msys64\usr\bin\bash.exe -lc 'cd /d/MyOS/bootloader && tools/misc/build_win.sh <target>'`。WorkBuddy 自带 Git Bash 把 `/msys64` 解析成 `E:\Program Files\Git\msys64`（不存在），导致 `make: not found`；真实 MSYS2 的 `/msys64/usr/bin` 才含 make/nasm。交叉编译器由 build_win.sh 的 PATH(`/c/Users/trans/elf_tools/bin`)补上。
- 本机 WHPX 不可用 → 所有 QEMU 验证一律 `-accel tcg`；内存紧用 `-m 256 -accel tcg,tb-size=128`（默认 1GB JIT 触发 cannot set up guest memory）。
- **本机无原生 C 编译器**（MSYS2 各工具链目录下均无 gcc/clang，elf_tools 只有交叉编译器产出 ELF 不能直接执行）。要对 C 逻辑做确定性实测时，把逻辑 1:1 移植成 Python，用托管 python `C:\Users\trans\.workbuddy\binaries\python\versions\3.13.12\python.exe` 跑断言（例：`tools/verify/test_hfilename.py` 验证 `h_file_name` 空格截断修复）。
- **后台代理 WIP 可能打断构建**：工作树里 net.cpp/linux_compat.cpp 等常有未提交半成品改动，会出现 `net_init` 缺声明、`case` 号撞车等编译错误。临时修复（加前向声明 / 临时改系统调用号）拿到可启动镜像验证后**务必回滚**，保留代理的 WIP 原状，撞车/重排决定留给对应代理。
- 默认 `os.img` BIOS raw（7.4MiB）；Linux 用户分区 `LINUX_SFS_LBA=12288`（Makefile+kernel.cpp 同步）；UEFI 诊断盘 `os_uefi_diag.img`。

## 构建/测试纪律（用户强制）
- 改动后必须实测：构建 + 无头 QEMU serial 抓内核标记/截图，不能只编译过。
- 完成先发"验证问答"，用户 OK 才收工。渐进式推进，不擅自跳阶段。

## GUI 交互审查协作命令（用户强制，2026-10-11 起）
- 任务来源：GUI 交互审查与优化代理，目标 = QEMU 内 NexOS GUI "像 Windows" 交互 + 鼠标移动绝对无重影/残影/双光标。
- 工作流：**用户让我一次性把报告里所有 bug 都改掉**；改完后**用户逐个查看是否丢失/复现**。
- 每修复一个 bug，代理**必须问用户一个问题**（确认该修复的验证结果 / 是否还有问题），**用户回答后代理才继续修下一个**；**禁止**不等回答就连续修多个。
- 若某 bug **未修复成功**，用户会告知**出现了什么新问题**；代理据此继续，不擅自换方向。
- 诊断报告固定存于 `GUI_BUG_DIAGNOSIS.md`（含 A 类=鼠标重影、B 类=卡顿、C 类=菜单渲染）。
- 约束：不引入外部 GUI 库（LVGL/SDL2 等），继续帧缓冲自绘；不改 kernel64.cpp 核心逻辑；GUI 改动集中在 gui.cpp 与启动脚本；每次 git 提交 message 写明根因/修法/验证。
- 注意：本机当前可用 **WSL（Ubuntu-24.04）** 构建（`wsl -e bash -lc "cd /mnt/e/NexOS/NexOS && make ..."` 已验证通过），与上文"纯 Windows 不用 WSL"的旧约定已变化——构建方式以实际可用为准。

## GUI 改动纪律（C# 优先，2026-08-20）
- 真实 VM 桌面由 C# 托管壳渲染（gui.cpp render_all 内 managed_desk=true）；桌面/任务栏/菜单改 NexOS.Forms/apps/Shell/*，烤进 shell.mex→sfs.img→镜像。窗口 chrome 仍是原生 gui.cpp::draw_window。
- 渲染按需：gui_tick() 仅 g_mforms_anim 时 render_all()，空闲零渲染。改共享源后 shell.mex 与 csharp/winhost 都要重编。

## 真 VM 登录与桌面验证
- 账号 root/admin(uid0)、guest/guest(uid1000)，启动进 Win11 锁屏；无头登录 sendkey `admin`+ret。验证脚本 capture_desk.py / qemu_uefi_verify_diag.py。

## 高频坑（内核/编译/链接）
- gfx 半透明用 blend_rect/blend_rounded_rect；16bpp 掩码 `((c>>19)&0x1F)<<11|((c>>10)&0x3F)<<5|(c>>3)&0x1F`。
- CLR 字符串字面量上限 clr.cpp CLR_MAX_STRINGS（256→512，报 too many 继续上调+扩 g_strobj）。
- 无宿主 C 库 freestanding 源（net.cpp、.attic64/kernel64.cpp 等）用 NULL 前须 `#ifndef NULL #define NULL 0 #endif`。
- 32 位内核须 register_gui_callbacks()；64 位 IDT gate 按加载 delta 重定位（BIOS 实测 delta=0x7）。

## UEFI / 高帧缓冲黑屏（根因已修，备查）
- 真机 GOP LFB 常 >4GB；.compat_path 须先装 0x90000 新页表切 CR3 再 blit（否则拷贝覆盖在用页表→triple fault）。长模式页表 512 项×8B（非1024）。vmm_init() 须提前到 GDT/IDT/PIC 后。
- boot_beacon() RGBX/BGRX 打包：RGBX `(b<<16)|(g<<8)|r`、BGRX `(r<<16)|(g<<8)|b`（真机/QEMU 均 BGRX32）。

## 64 位诊断与落盘 / CD 早期崩溃（悬挂未修，备查）
- 诊断落盘扇区 LBA1500（原 LBA34 落在 kernel.bin 内会污染）；kmain64 早调 ata_write_sector 会硬冻结，diag_step 落盘仅步号≥100。
- os.iso 真机引导后内核标记从不出现（BIOS raw 路径正常）；嫌疑 boot_cd.asm 保护模式远跳转 16:16 vs 16:32。修前先清残留调试探针。

## distnet 分布式算力网络（2026-08-21 起，2026-08-26 打通真推理）
- 任务 sum/echo/compute + ai（派发原生 AI 引擎）。协议 UDP 换行 ASCII：QUERY/BEACON(5455) TASK(5456) RESULT(5457)。**端口原 5355 是 IANA LLMNR 被 svchost 独占，已迁 5455/5456/5457**，改须同步 distnet.h+distnet.cpp+tools/net/distnet_host_peer.py。
- 新增 `distnet ask "<question>"`：发现所有 compute 节点 → 同问题派发给多台 VM → 收集并合并答案（演示多机算力合并）。RESULT/answer 缓冲已放宽到 400B 容纳真推理答案。
- **真推理打通（2026-08-26）**：`.attic64/kernel64.cpp` 的 `kern_ai_ask` 改为优先走 64 位 `qwen_generate`（读 LBA16384 的 GGUF blob + 真实 transformer 前向），绕过 VM 检测的 Markov 离线引擎；`distnet compute` 收到 `ai` 任务即触发真推理。模型用 **Qwen2-0.5B-Instruct Q4_K_M**（因 gguf_infer.cpp 仅支持 qwen2/llama，不支持 Phi-3；2GB VM 也跑得动 0.5B）。
- **演示镜像**：`make build/os.img MODEL_GGUF=build/qwen2-0_5b-instruct-q4_k_m.gguf`（须先 `rm -f build/os.img` 强制重建，否则 make 认为 up-to-date 不跑 embed 步）。产物 `build/os.img` ≈387MB（含 397MB 模型 blob @LBA16384/描述符 LBA16383）。
- **演示运行必须用 Windows 端 QEMU**：`D:\qemu\qemu-system-x86_64.exe`（当前 WSL 环境**无 qemu 且 apt 源不可达无法安装**）。3 VM（1 scheduler+2 compute，各 `-m 2G -accel tcg`）走真实 L2 UDP 隧道组网，脚本 `tools/verify/test_distnet_ask.py`。本机 WHPX 不可用 → 一律 `-accel tcg`。

## Linux 兼容层路线（当前主线，2026-08 起，远期目标跑真·MC Java 版）
- freestanding ELF32 guest 跑 RING0；int 0x80 真实入口 sys_enter(syscall.cpp:178)。32 位非 PAE 分页，4MiB PSE identity-map **0–32MiB 给内核**(含内核栈 0x1800000–0x1810000、RAM-SFS 0x1400000–0x1800000、.bss 0x120000–0x200000) + **64–256MiB 为 PG_USER 空闲区**；**无 NX→用户页天然可执行**（W^X 软约束）。
- 构建 guest：NEX_CC=i686-elf-g++ `-x c -m32 -ffreestanding -nostdlib -fno-stack-protector`；链接 `-m elf_i386 -nostdlib -Ttext=$(NEX_TEXT) -e _start`，**NEX_TEXT=0x08048000**（落在 64–256MiB PG_USER 区，与 `linux_run` 既有布局注释 ELF@0x08048000/栈@0xC000000/mmap@0xC100000–0x0FFFF000 一致）。**严禁改回 0x01800000**——那会覆盖内核启动栈（linker.ld 2026-08-19 把栈从 0x90000 迁到 0x1800000 后，旧基址 0x01800000 与之冲突，加载客户机时清零 bss 把内核栈帧 i/phnum/返回地址清零→PH 循环洪泛、静默卡死）。
- g_reader 从 linux_fs(挂载于 LBA 12288, SFS_LINUX_LBA) 读文件，未挂载则回退主 sfs；cmd_linux 解析内核命令行 argv 传 linux_run(name,ac,av)；execve(case 11) 真·加载 ELF + 透传 argv + 透传 envp(r->edx)（envp 解析在 linux_compat.cpp sys_execve 分支，落盘前先剥离前导 `/`）。
- 阶段进度：S1 基础加载/hello；S2 信号(handler sig_slot GAP=88、clone 拷贝 sa[]/blocked、worker yield)；S3 mmap/mprotect/munmap+扩大竞技场(PASS 10/10)；**S4 DONE(PASS) — argv 从 `linux` 命令行到达 main()；execve 的 envp(r->edx) 解析并透传新映像（自举 `linux linux_argv spawn` 验证 STAGE4=ENV_PASSED/FOO=BAR）**。
- 验证：tools/verify/verify_linux_argv.py 无头 QEMU(`-m 256 -accel tcg,tb-size=128`)，从 shell 输入 `linux <prog> [args]`，抓 serial `LXARG:` 标记判定（8 项全 PASS）。**偶发毛刺**：shell-ready 后立即首访 reader 偶发首次 `linux` 命令 file not found、二次成功；干净复跑可 8/8 PASS，与 NEX_TEXT 改动无关，待查（疑 g_fsbuf 共享/ATA 首读时序）。
