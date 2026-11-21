package dev.mygo

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.os.Handler
import android.os.Looper
import org.json.JSONObject
import java.net.Inet4Address

@Suppress("DEPRECATION")
class GalScalerDiscovery : MyGoPlugin {
    private val manager get() = PluginHost.context.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val main = Handler(Looper.getMainLooper())
    private val servers = LinkedHashMap<String, String>()
    private val pending = LinkedHashMap<String, NsdServiceInfo>()
    private val visible = mutableSetOf<String>()
    private var running = false
    private var resolving = false

    private val discovery = object : NsdManager.DiscoveryListener {
        override fun onDiscoveryStarted(type: String) {}
        override fun onDiscoveryStopped(type: String) { main.post { reset() } }
        override fun onStartDiscoveryFailed(type: String, error: Int) { main.post { reset() } }
        override fun onStopDiscoveryFailed(type: String, error: Int) {}
        override fun onServiceLost(service: NsdServiceInfo) {
            main.post {
                visible.remove(service.serviceName)
                servers.remove(service.serviceName)
                pending.remove(service.serviceName)
            }
        }
        override fun onServiceFound(service: NsdServiceInfo) {
            main.post {
                visible.add(service.serviceName)
                pending[service.serviceName] = service
                resolveNext()
            }
        }
    }

    override fun call(method: String, args: JSONObject, reply: PluginReply) {
        if (method != "list") {
            reply.failure(PluginFailure("NoMethod", method))
            return
        }
        if (!running) {
            manager.discoverServices("_galscaler._tcp.", NsdManager.PROTOCOL_DNS_SD, discovery)
            running = true
        }
        reply.success(JSONObject(servers.toMap()))
    }

    private fun reset() {
        running = false
        servers.clear()
        visible.clear()
        pending.clear()
    }

    private fun resolveNext() {
        if (resolving) return
        val (name, service) = pending.entries.firstOrNull() ?: return
        pending.remove(name)
        resolving = true
        manager.resolveService(service, object : NsdManager.ResolveListener {
            override fun onResolveFailed(info: NsdServiceInfo, error: Int) {
                main.post { complete() }
            }
            override fun onServiceResolved(info: NsdServiceInfo) {
                main.post {
                    val host = if (Build.VERSION.SDK_INT >= 34) {
                        info.hostAddresses.firstOrNull { it is Inet4Address } ?: info.hostAddresses.firstOrNull()
                    } else info.host
                    val address = info.attributes["address"]?.toString(Charsets.UTF_8) ?: host?.hostAddress
                    if (name in visible && address != null && info.port > 0 && !address.contains('%')) {
                        val target = if (address.contains(':')) "[$address]" else address
                        servers[name] = "$target:${info.port}"
                    }
                    complete()
                }
            }
            private fun complete() {
                resolving = false
                resolveNext()
            }
        })
    }
}
