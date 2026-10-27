# 代码执行隔离

`bodyConversion.codeExecution` 打开后，代理会在本机执行模型生成的 bash 或 python 代码。代码默认在代理自己的容器里运行，身份是 root，能读到 `/app/config/credentials.json` 和环境里的所有密钥。在完成下面的隔离之前，不要对任何外部可达的路由打开这个开关。

## 现在已有的防护

- 执行放在 blocking 线程池里，单次执行不会卡住代理的异步线程。
- 子进程在独立的进程组里运行。超时后整组一起 kill，后台子进程不能让请求一直挂着。
- 环境变量会被清空，工作目录是每次新建的临时目录。
- `tool_choice: none` 时不执行，流式和非流式都先过工具门禁。
- 单次超时、代码大小、输出大小有上限：`timeoutMs`、`maxCodeBytes`、`maxOutputBytes`。

这些措施只能防止误伤和资源占用，挡不住恶意代码，所以还需要做进程隔离。

## 发布时要做的隔离

做法：在代理容器旁边单独起一个执行容器，代码在执行容器里跑。执行容器里没有凭据、没有网络、不是 root。代理容器也不挂 docker socket，否则代理容器一旦被攻破就等于拿到了宿主机。

1. **新建执行镜像**：基于 `python:3-alpine`，带上 bash，创建非 root 用户 `runner`（uid 10001），只包含 `/usr/bin/env`、`sh`、`bash`、`python3`。不要复制任何代理文件或配置进去。

2. **compose 新增服务 `code-runner`**：
   - `user: "10001:10001"`
   - `network_mode: none`：不能访问网络，也碰不到代理、PostgreSQL 和 Redis。
   - `read_only: true`，再加 `tmpfs: ["/work:size=64m,mode=1777"]`，作为唯一可写的目录。
   - `cap_drop: [ALL]`、`security_opt: ["no-new-privileges:true"]`
   - `pids_limit: 64`、`mem_limit: 256m`、`cpus: "0.5"`
   - 不挂载 `/app/config`、`/app/logs`、docker socket 或任何宿主机目录。
   - 不配置任何环境变量。

3. **代理到执行容器的调用方式**：执行容器没有网络，所以通过共享的 Unix socket 通信。
   - 在两个容器之间共享一个只放 socket 的命名卷，比如 `runner-sock:/run/runner`。
   - 执行容器里跑一个最小的执行服务，在 `/run/runner/exec.sock` 上接收 `{language, code, timeoutMs}`，在 `/work` 下执行，返回 `{stdout, stderr, exitCode, timedOut}`。超时 kill 进程组的逻辑和代理现在的实现一样。
   - 代理里 `code_execution::execute` 从本地 `Command` 改为请求这个 socket。socket 不可用时返回执行失败，不要回退到本地执行。
   - 新增配置项 `bodyConversion.codeExecution.runnerSocket`，例如 `/run/runner/exec.sock`。为空时代码执行保持关闭，即使 `enabled` 或 `pathOverrides` 为 true 也不执行。

4. **代理容器本身也收紧**：Dockerfile 里加上非 root 用户，以 `USER kiro` 运行代理，`/app/config` 只读挂载。这一步和代码执行无关，但能减小被攻破后的影响面。

5. **上线检查**：在执行容器里依次运行 `cat /app/config/credentials.json`、`wget http://kiro-rs:8990`、`id`、`touch /etc/x`、`sleep 999 &`，预期结果分别是：文件不存在、网络不通、uid 10001、只读文件系统报错、超时后进程被清理。

## 隔离完成之前

- 现网保持 `codeExecution.enabled=false`，`pathOverrides` 里不保留任何 `true`。
- 本地验证用隔离实例，不要连现网凭据。
