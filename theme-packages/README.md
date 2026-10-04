# 第三方主题包

此目录用于分发主题包，不在默认主题扫描目录内，也不进入 Docker 运行镜像。
系统只内置 `themes/default`。

## Paper

Paper 源码位于 [paper/](paper/)，可打包为直接上传的 `paper.zip`；ZIP 分发包不纳入 Git。
ZIP 内包含 `paper/theme.json`、`paper/templates/` 和 `paper/assets/`，安装后的 slug 为 `paper`。

在后台「主题管理」选择 ZIP，可先验证，再安装、选择并激活；无需重启。
如需测试卸载，先激活其他主题，再卸载 Paper。

打包可在仓库根目录执行：

```sh
cd theme-packages
zip -r paper.zip paper
```

已有 Docker 命名卷中的 Paper 文件会继续保留。测试重新安装前，先切换到 Default，
再从后台卸载已有 Paper；仅更新镜像不会删除已有主题卷中的文件。
