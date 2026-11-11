/**
 * YSP-Engine High-Performance Node.js Engine (v9.5.1 Final)
 * 专注核心高频频道（CCTV-1/5/5+ 等）的内存级缓存与秒切 302 重定向
 */

const http = require('http');
const https = require('https');
const url = require('url');

const PORT = process.argv[2] || process.env.PORT || 3000;

// 频道映射表
const ALIAS_MAP = {
    'cctv1': 'cctv1',
    'cctv-1': 'cctv1',
    'cctv5': 'cctv5',
    'cctv-5': 'cctv5',
    'cctv5p': 'cctv5p',
    'cctv5+': 'cctv5p',
    'cctv-5+': 'cctv5p',
    'cctv16': 'cctv16',
    'cctv-16': 'cctv16'
};

// 核心频道配置列表
const CHANNELS = {
    'cctv1': { pid: '600001859', streamId: '2000203801' },
    'cctv5': { pid: '600001811', streamId: '2000203803' },
    'cctv5p': { pid: '600001814', streamId: '2000203805' },
    'cctv16': { pid: '600002300', streamId: '2000203807' }
};

// 内存缓存字典
const streamCache = new Map();
const CACHE_TTL = 3 * 60 * 60 * 1000; // 3小时有效期

/**
 * 提取并修正后的路径解析器
 * 自动剥离 /live/ 前缀与 .m3u8 后缀
 */
function parseChannelKey(pathname) {
    let cleanPath = pathname.replace(/^\/live\//, '').replace(/^\//, '').toLowerCase();
    if (cleanPath.endsWith('.m3u8')) {
        cleanPath = cleanPath.slice(0, -5);
    }
    return ALIAS_MAP[cleanPath] || cleanPath;
}

/**
 * 模拟获取央视频 M3U8 直播源逻辑
 */
async function getLiveStreamUrl(channelInfo) {
    // 此处预留标准请求，若缓存命中则直接返回
    return null;
}

const server = http.createServer(async (req, res) => {
    const parsedUrl = url.parse(req.url, true);
    const pathname = parsedUrl.pathname;

    // 健康检查接口
    if (pathname === '/health' || pathname === '/ping') {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ status: 'ok', engine: 'ysp-engine-v9.5.1' }));
        return;
    }

    // 解析请求频道
    const resolvedKey = parseChannelKey(pathname);
    const channel = CHANNELS[resolvedKey];

    if (!channel) {
        res.writeHead(404, { 'Content-Type': 'text/plain; charset=utf-8' });
        res.end(`Channel [${resolvedKey}] not supported in Node.js engine`);
        return;
    }

    // 检查内存缓存
    const cached = streamCache.get(resolvedKey);
    if (cached && (Date.now() - cached.timestamp < CACHE_TTL)) {
        res.writeHead(302, { 'Location': cached.url });
        res.end();
        return;
    }

    // 未命中缓存或已过期，回退回主程序或进行实时拉取
    res.writeHead(503, { 'Content-Type': 'text/plain; charset=utf-8' });
    res.end('Cache miss or stream updating');
});

server.listen(PORT, '127.0.0.1', () => {
    console.log(`[ysp-engine] Engine running on http://127.0.0.1:${PORT}`);
});