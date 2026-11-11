#!/bin/sh
# ysp-live v9.5.1 —— Rust 版一键部署（保留原版交互菜单与 host 网络模式）

CONTAINER_NAME="ysp-live"
INSTALL_DIR="/root/ysp-live"
PORT="8767"

get_local_ip() {
    ip=$(uci get network.lan.ipaddr 2>/dev/null)
    if [ -z "$ip" ]; then
        ip=$(hostname -I 2>/dev/null | awk '{print $1}')
    fi
    if [ -z "$ip" ]; then
        ip="localhost"
    fi
    echo "$ip"
}

IP=$(get_local_ip)

show_links() {
    echo ""
    echo "===================================================================="
    echo "  📌 ysp-live 服务访问与管理链接："
    printf "   • 全频道订阅地址: \033[1;32mhttp://%s:%s/all.m3u\033[0m\n" "$IP" "$PORT"
    printf "   • 系统健康度检查: \033[1;32mhttp://%s:%s/health\033[0m\n" "$IP" "$PORT"
    printf "   • 实时诊断与日志: \033[1;32mhttp://%s:%s/diag\033[0m\n" "$IP" "$PORT"
    echo "===================================================================="
    echo ""
}

do_install() {
    echo "===================================================================="
    echo "  🚀 开始部署 ysp-live (Rust 版)..."
    echo "===================================================================="

    if ! command -v docker >/dev/null 2>&1; then
        printf "\033[1;31m[错误] 未检测到 Docker，请先在 OpenWrt/Linux 中安装 Docker。\033[0m\n"
        exit 1
    fi

    mkdir -p ${INSTALL_DIR} && cd ${INSTALL_DIR}

    echo "[1/2] 清理旧容器并构建启动..."
    docker rm -f ${CONTAINER_NAME} 2>/dev/null
    docker compose up -d --build

    if [ $? -eq 0 ]; then
        printf "\033[1;32m[2/2] ysp-live 部署成功！\033[0m\n"
        show_links
    else
        printf "\033[1;31m[错误] 容器启动失败，请检查 Docker 日志。\033[0m\n"
    fi
}

if [ "$(docker ps -a -q -f name=${CONTAINER_NAME})" ]; then
    echo "===================================================================="
    echo "  检测到系统中已存在 ${CONTAINER_NAME} 容器实例"
    echo "===================================================================="
    echo " 请选择要执行的操作："
    echo "   1) 📄 显示当前服务的访问与订阅链接"
    echo "   2) 📊 查看实时运行日志 (Docker Logs)"
    echo "   3) 🩺 本地检查系统健康度 (Health API)"
    echo "   4) 🔄 重新安装 / 更新到最新版本 (Reinstall)"
    echo "   5) ❌ 退出脚本"
    echo "===================================================================="
    printf "请输入选项编号 [1-5]: "
    read choice

    case "$choice" in
        1) show_links ;;
        2)
            echo "正在进入实时日志流 (按 Ctrl+C 可退出日志查看)..."
            docker logs -f ${CONTAINER_NAME}
            ;;
        3)
            echo "正在请求本地健康接口..."
            if command -v curl >/dev/null 2>&1; then
                curl -s http://127.0.0.1:${PORT}/health
                echo ""
            else
                wget -qO- http://127.0.0.1:${PORT}/health
                echo ""
            fi
            ;;
        4)
            echo "即将开始重新安装与更新..."
            do_install
            ;;
        *)
            echo "已安全退出。"
            exit 0
            ;;
    esac
else
    echo "===================================================================="
    echo "  未检测到 ${CONTAINER_NAME} 运行实例"
    echo "===================================================================="
    printf "是否立即开始自动安装？[y/N]: "
    read confirm
    case "$confirm" in
        [yY] | [yY][eE][sS]) do_install ;;
        *) echo "已取消安装。" ; exit 0 ;;
    esac
fi
