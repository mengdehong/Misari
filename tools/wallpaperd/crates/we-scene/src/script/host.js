'use strict';
let __weNodes=[], __weScripts=[], __weCurrent=-1, __weOverrides={};
let __weCurrentEvent='load';
globalThis.thisLayer=undefined;globalThis.thisObject=undefined;
const __weDirty=new Map(), __weTimers=new Set(), __weAudioViews=new Map();
const __weTracked=new WeakMap();
const __weAudioOwners=new Set();
function __weActive(index){return index>=0&&!!__weScripts[index]&&!__weScripts[index].disabled&&!__weNodes[__weScripts[index].node].__destroyed;}
function __weFeatures(){
    // Private host bits: animated=1, audio=2, mediaTimeline=4, pointer=8.
    let features=0,active=false;
    for(let i=0;i<__weScripts.length;i++)if(__weActive(i)){
        active=true;const ns=__weScripts[i].namespace;
        if(typeof ns.update==='function')features|=1;
        if(typeof ns.mediaTimelineChanged==='function')features|=4;
        if(['cursorEnter','cursorLeave','cursorMove','cursorDown','cursorUp','cursorClick'].some(e=>typeof ns[e]==='function'))features|=8;
    }
    for(const timer of __weTimers)if(timer.owner<0||__weActive(timer.owner)){features|=1;break;}
    for(const owner of __weAudioOwners)if(owner<0?active:__weActive(owner)){features|=2;break;}
    return features;
}
let __weSyncing=false;
let __weSceneIndex=-1;
class CameraTransforms {constructor(){this.eye=new Vec3(0,0,1000);this.center=new Vec3(0);this.up=new Vec3(0,1,0);this.zoom=1;}}
const __weDefaultScene={cameraTransforms:new CameraTransforms(),clearcolor:new Vec3(0),fov:50,nearz:0.01,farz:10000};
function __weSettings(){return __weSceneIndex>=0?__weNodes[__weSceneIndex]:__weDefaultScene;}
const __weVectorFields={origin:3,angles:3,scale:3,color:3,point0:3,point1:3,textcolor:3,backgroundcolor:3,outlinecolor:3,dropshadowcolor:3,dropshadowoffset:2,size:2,parallaxDepth:2,clearcolor:3,ambientcolor:3,skylightcolor:3};
let __weLastPointer={focused:false,down:false,hits:[],pressed:[],position:[0,0]};
function __weCancelPointer(){__weLastPointer.pressed=[];__weLastPointer.cancelled=true;}
let __weLastTimeline;
function __weVector(value,n) {
    const type=n===2?Vec2:n===4?Vec4:Vec3;
    return value instanceof type?value:new type(...(Array.isArray(value)?value:[value]));
}
function __weCheck(index) {
    if(__weSyncing)return;
    if(!__weNodes[index] || __weNodes[index].__destroyed) throw new ReferenceError('Scene layer handle has been destroyed');
}
function __weMark(index,path,value) {
    if(__weSyncing) return;
    const own=JSON.stringify([index,path]);
    if(__weDirty.has(own)){__weDirty.set(own,[index,path,value]);return;}
    for(const [key,[node,existing]] of __weDirty) {
        if(node!==index)continue;
        if(existing.length<path.length&&existing.every((v,i)=>path[i]===v))return;
        if(path.length<existing.length&&path.every((v,i)=>existing[i]===v))__weDirty.delete(key);
    }
    __weDirty.set(own,[index,path,value]);
}
function __weTrackedProxy(value,index,path,handlers) {
    const proxy=new Proxy(value,handlers);
    __weTracked.set(proxy,{index,path});
    return proxy;
}
function __weTrack(value,index,path) {
    // Property scripts commonly return their input. Keep that handle instead of
    // adding another setter trap on every frame.
    const tracked=__weTracked.get(value);
    if(tracked&&tracked.index===index&&tracked.path.length===path.length&&tracked.path.every((key,i)=>key===path[i]))return value;
    if(value instanceof WEVec) return __weTrackedProxy(value,index,path,{
        set(target,key,v){ __weCheck(index);target[key]=v;__weMark(index,path,target);return true; },
    });
    if(value && typeof value==='object') {
        __weInstallEffect(value,index,path);
        __wePrepareMaterial(value,path);
        __weAttachAnimations(value,index,path);
        for(const key of Object.keys(value)) {
            const child=value[key];
            value[key]=child&&typeof child==='object'?__weTrack(child,index,path.concat(key)):child;
        }
        return __weTrackedProxy(value,index,path,{
            get(target,key){__weCheck(index);return target[key];},
            set(target,key,v){
                __weCheck(index);
                // Scalar host samples cannot acquire tracked children. Avoid
                // allocating paths for setters whose dirty mark is suppressed.
                if(__weSyncing&&(v===null||typeof v!=='object')){target[key]=v;return true;}
                const field=path.concat(key);
                target[key]=__weTrack(v,index,field);
                if(Array.isArray(target))__weMark(index,path,target);else __weMark(index,field,v);
                return true;
            },
        });
    }
    return value;
}
function __weMakeLayer(raw,index) {
    if(raw.__scene) {
        raw.cameraTransforms??={};
        const defaults=new CameraTransforms();
        for(const key of ['eye','center','up']) raw.cameraTransforms[key]=__weVector(raw.cameraTransforms[key]??defaults[key],3);
        raw.cameraTransforms.zoom??=1;
    }
    for(const [key,n] of Object.entries(__weVectorFields)) {
        if(raw[key]!==undefined) {
            raw[key]=__weVector(raw[key],n);
            if(key==='angles') raw[key]=raw[key].multiply(180/Math.PI);
        }
    }
    raw.origin??=new Vec3(0);raw.angles??=new Vec3(0);raw.scale??=new Vec3(1);raw.color??=new Vec3(1);raw.visible??=true;
    if(raw.light) raw.castshadow??=false;
    else if(raw.model!==undefined) raw.castshadow??=true;
    __weInstallModel(raw,index);
    __weInstallSound(raw,index);
    __weInstallParticle(raw,index);
    __weInstallTexture(raw,index);
    __weInstallMaterials(raw,index);
    __weAttachAnimations(raw,index,[]);
    Object.defineProperties(raw,{
        __index:{value:index}, __destroyed:{value:false,writable:true}, __pendingDestroy:{value:false,writable:true},
        getParent:{value:()=>{__weCheck(index);return thisScene.getLayerByID(raw.parent);}},
        getChildren:{value:()=>{__weCheck(index);return __weNodes.filter(n=>!n.__destroyed&&n.parent===raw.id);}},
        getTransformMatrix:{value:()=>{__weCheck(index);return __weLayerWorld(index);}},
        setParent:{value:(parent,attachment,adjust)=>__weSetParent(index,parent,attachment,adjust)},
    });
    for(const key of Object.keys(raw)) raw[key]=__weTrack(raw[key],index,[key]);
    return new Proxy(raw,{
        get(target,key){if(key!=='__destroyed'&&key!=='__pendingDestroy'&&key!=='__index') __weCheck(index);return target[key];},
        set(target,key,v){
            __weCheck(index);
            if(key==='__pendingDestroy'){target[key]=v;return true;}
            if(__weVectorFields[key]) v=__weVector(v,__weVectorFields[key]);
            if(key==='__texture'&&target.__videoMaster===index)__weVideoStates.set(index,v);
            if(__weSyncing&&(v===null||typeof v!=='object')){target[key]=v;return true;}
            const path=[key];target[key]=__weTrack(v,index,path);__weMark(index,path,v);return true;
        },
    });
}
function __wePath(object,path) { for(const key of path) object=object[key];return object; }
function __weSetPath(object,path,value) {
    const parent=__wePath(object,path.slice(0,-1)),key=path[path.length-1];
    if(parent[key] instanceof WEVec) value=__weVector(value,parent[key]._n);
    parent[key]=value;
}
function __weActivate(index) {
    __weCurrent=index;
    const script=__weScripts[index];
    globalThis.thisLayer=__weNodes[script.node];
    globalThis.thisObject=__wePath(thisLayer,script.path.slice(0,-1));
}
function __weReserve(node,path,overrides) {
    if(__weScripts.length>=512)throw new RangeError('SceneScript module budget exceeded');
    const index=__weScripts.length;__weScripts.push({namespace:{},node,path,disabled:false,initialized:false});
    __weActivate(index);__weOverrides=overrides;return index;
}
function __weAttach(namespace,node,path,overrides,index) {
    __weScripts[index].namespace=namespace;
    const current=__wePath(__weNodes[node],path);
    if(typeof current==='string'&&path[path.length-1]!=='text') {
        const values=current.trim().split(/\s+/).map(Number);
        if(values.length>=2&&values.length<=4&&values.every(Number.isFinite)) __weSetPath(__weNodes[node],path,__weVector(values,values.length));
    }
    if(namespace.scriptProperties) for(const [key,value] of Object.entries(overrides)) {
        const old=namespace.scriptProperties[key];
        namespace.scriptProperties[key]=old instanceof WEVec?__weVector(value,old._n):value;
    }
}
function __weCall(index,event,arg) {
    const script=__weScripts[index];
    if(script.disabled || __weNodes[script.node].__destroyed) return;
    const fn=script.namespace[event];
    if(event==='init'){if(script.initialized)return;script.initialized=true;}
    if(typeof fn!=='function') return;
    __weActivate(index);
    const previousEvent=__weCurrentEvent;__weCurrentEvent=event;
    try {
        const property=event==='init'||event==='update';
        const result=fn(property?__wePath(thisLayer,script.path):arg,__wePath(thisLayer,script.path));
        if(property&&result!==undefined) __weSetPath(thisLayer,script.path,result);
    } catch(error) {
        script.disabled=true;
        __weLog('SceneScript '+index+' '+event+': '+String(error)+'\n'+String(error.stack||''));
    }
    finally {__weCurrentEvent=previousEvent;}
}
function __weEvent(event,arg) {
    if(event==='mediaThumbnailChanged') for(const key of ['primaryColor','secondaryColor','tertiaryColor','textColor','highContrastColor']) arg[key]=__weVector(arg[key],3);
    for(let i=0;i<__weScripts.length;i++) __weCall(i,event,arg);
}
class MediaPlaybackEvent {
    static PLAYBACK_STOPPED=0;
    static PLAYBACK_PLAYING=1;
    static PLAYBACK_PAUSED=2;
}
function __weFlush() {
    const deltas=__weCreated.splice(0).map(([index,definition])=>[index,[],{definition,values:{...__weNodes[index],angles:__weNodes[index].angles.multiply(Math.PI/180)}}]);
    for(const [,entry] of __weDirty) {
        let [index,path,value]=entry;
        if(path.length===1 && path[0]==='angles' && value instanceof WEVec) value=value.multiply(Math.PI/180);
        deltas.push([index,path,value]);
    }
    __weDirty.clear();
    for(let index=0;index<__weNodes.length;index++) {
        const node=__weNodes[index];
        if(node.__pendingDestroy && !node.__destroyed) {
            for(let i=0;i<__weScripts.length;i++) if(__weScripts[i].node===index) __weCall(i,'destroy');
            // No script observes a half-destroyed object during a frame.
            Object.defineProperty(node,'__destroyed',{value:true});
            deltas.push([index,['visible'],false]);
            deltas.push([index,['__destroyed'],true]);
        }
    }
    __weSyncing=true;
    try{return __weSerialize(deltas);}finally{__weSyncing=false;}
}
const shared={};
const console={};
for(const method of ['log','info','warn','error','debug']) console[method]=(...args)=>__weLog('console.'+method+': '+args.map(v=>String(v)).join(' ').slice(0,2048));
const input={cursorWorldPosition:new Vec3(0),cursorScreenPosition:new Vec2(0),cursorLeftDown:false};
const engine={
    AUDIO_RESOLUTION_16:16,AUDIO_RESOLUTION_32:32,AUDIO_RESOLUTION_64:64,
    runtime:0,frametime:0,timeOfDay:0,userProperties:{},screenResolution:new Vec2(0),canvasSize:new Vec2(0),
    registerAudioBuffers(count){
        if(![16,32,64].includes(count)) throw new RangeError('Audio resolution must be 16, 32 or 64');
        __weAudioOwners.add(__weCurrent);
        if(!__weAudioViews.has(count)) __weAudioViews.set(count,{left:new Float32Array(count),right:new Float32Array(count),average:new Float32Array(count)});
        return __weAudioViews.get(count);
    },
    registerAsset(file){__weValidateAsset(file);return Object.freeze({file});},
    isRunningInEditor:()=>false,isDesktopDevice:()=>true,isMobileDevice:()=>false,isWallpaper:()=>true,isScreensaver:()=>false,
    isPortrait:()=>engine.screenResolution.y>engine.screenResolution.x,isLandscape:()=>engine.screenResolution.x>=engine.screenResolution.y,
    setTimeout:(callback,delay=0)=>__weTimer(callback,delay,false),
    setInterval:(callback,delay=0)=>__weTimer(callback,delay,true),
};
function __weTimer(callback,delay,repeat) {
    if(typeof callback!=='function'||!Number.isFinite(delay)||__weTimers.size>=512) throw new RangeError('Invalid or excessive SceneScript timers');
    const timer={callback,delay:Math.max(0.001,delay/1000),due:engine.runtime+Math.max(0,delay/1000),repeat,owner:__weCurrent};
    __weTimers.add(timer);return ()=>__weTimers.delete(timer);
}
const thisScene=new Proxy({
    getLayer(key){return typeof key==='number'?this.enumerateLayers()[key]:this.enumerateLayers().find(n=>n.name===key||n.id===key);},
    getLayerByID(id){return __weNodes.find(n=>!n.__destroyed&&!n.__scene&&String(n.id)===String(id));},
    getLayerCount(){return this.enumerateLayers().length;},
    enumerateLayers(){return __weLayerOrder.map(i=>__weNodes[i]).filter(n=>n&&!n.__destroyed&&!n.__scene);},
    getLayerIndex(layer){return this.enumerateLayers().indexOf(typeof layer==='object'?layer:this.getLayer(layer));},
    getInitialLayerConfig(layer){const node=typeof layer==='object'?layer:this.getLayer(layer);if(!node)return undefined;__weCheck(node.__index);return JSON.parse(JSON.stringify(__weInitial[node.__index]));},
    createLayer:configuration=>__weCreateLayer(configuration),
    sortLayer(layer,index){const node=typeof layer==='object'?layer:this.getLayer(layer);if(!node||node.__destroyed||!Number.isInteger(index)||index<0||index>=this.getLayerCount())return false;__weLayerOrder=this.enumerateLayers().map(n=>n.__index);__weLayerOrder.splice(__weLayerOrder.indexOf(node.__index),1);__weLayerOrder.splice(index,0,node.__index);__wePublishOrder();return true;},
    getCameraTransforms(){const settings=__weSettings(),raw=settings.__cameraPose&&!settings.__cameraPose.__path&&!settings.__cameraOverride?settings.__cameraPose:settings.cameraTransforms;const out=new CameraTransforms();for(const key of ['eye','center','up'])out[key]=__weVector(raw[key],3).copy();out.zoom=raw.zoom;return out;},
    setCameraTransforms(value){const out=this.getCameraTransforms();for(const key of ['eye','center','up'])if(value[key]!==undefined){out[key]=__weVector(value[key],3).copy();if(!out[key].isFinite())throw new TypeError('Invalid camera vector');}if(value.zoom!==undefined){if(!(value.zoom>0)||value.zoom>1000||!Number.isFinite(value.zoom))throw new RangeError('Invalid camera zoom');out.zoom=value.zoom;}if(out.up.lengthSqr()<1e-12||out.eye.subtract(out.center).cross(out.up).lengthSqr()<1e-12)throw new RangeError('Invalid camera orientation');out.up=out.up.normalize();__weSettings().cameraTransforms=out;__weSettings().__cameraOverride=true;},
    destroyLayer(layer){const node=typeof layer==='object'?layer:this.getLayer(layer);if(!node||node.__destroyed)return false;node.__pendingDestroy=true;return true;},
    createModelData(config){return new IModelData(__weModelData('create',0,config),__weModelToken);},
    destroyModelData(handle){__weModelData('destroy',__weModelId(handle),null);},
},{get(target,key){return key in target?target[key]:__weSettings()[key];},set(target,key,value){if(key in target)throw new TypeError('Scene method is read-only');__weSettings()[key]=value;return true;}});
let __weInitial=[];
function __weSetInitial(raw){__weInitial=raw;}
function __weUserProperties(values,previous={}) {
    const result={};
    for(const [key,value] of Object.entries(values)) {
        if(Array.isArray(value)&&value.length===3&&value.every(Number.isFinite)) {
            const color=previous[key] instanceof Vec3?previous[key]:new Vec3(0);
            [color.x,color.y,color.z]=value;result[key]=color;
        } else result[key]=value;
    }
    return result;
}
function __weInitNodes(raw,size,properties) {
    __weInitial=JSON.parse(JSON.stringify(raw));
    __weNodes=raw.map((node,index)=>__weMakeLayer(node,index));
    __weSceneIndex=raw.findIndex(node=>node.__scene);
    __weLayerOrder=raw.map((_,i)=>i).filter(i=>i!==__weSceneIndex);
    engine.canvasSize.x=size[0];engine.canvasSize.y=size[1];engine.userProperties=__weUserProperties(properties);
}
function __weSyncNodes(raw) {
    function copy(target,values,root) {
        if(Array.isArray(target)&&Array.isArray(values))target.length=values.length;
        for(const [key,raw] of Object.entries(values)) {
            let value=raw;
            if(root&&__weVectorFields[key]) {
                value=__weVector(value,__weVectorFields[key]);
                if(key==='angles') value=value.multiply(180/Math.PI);
            }
            if(target[key] instanceof WEVec) {
                value=__weVector(value,target[key]._n);
                for(const component of ['x','y','z','w'].slice(0,value._n)) target[key][component]=value[component];
            } else if(value&&typeof value==='object'&&target[key]&&typeof target[key]==='object') copy(target[key],value,false);
            else target[key]=value;
        }
    }
    __weSyncing=true;
    try{for(let i=0;i<raw.length;i++) if(!__weNodes[i].__destroyed) copy(__weNodes[i],raw[i],true);}
    finally{__weSyncing=false;}
}
function createScriptProperties() {
    const values={};
    const builder={finish:()=>values};
    for(const method of ['addSlider','addCheckbox','addText','addCombo','addColor','addVec2','addVec3','addVec4']) builder[method]=definition=>{
        let value=Object.hasOwn(__weOverrides,definition.name)?__weOverrides[definition.name]:definition.value;
        if(method==='addColor'||method==='addVec3') value=__weVector(value,3);
        if(method==='addVec2') value=__weVector(value,2);
        if(method==='addVec4') value=__weVector(value,4);
        values[definition.name]=value;return builder;
    };
    return builder;
}
function __weClock(time) {
    engine.runtime=time;engine.frametime=0;
}
function __wePointer(frame) {
    input.cursorWorldPosition.x=frame.pointer[0];input.cursorWorldPosition.y=frame.pointer[1];
    input.cursorScreenPosition.x=frame.screenPointer[0];input.cursorScreenPosition.y=frame.screenPointer[1];input.cursorLeftDown=frame.down;
    const cancelled=!!__weLastPointer.cancelled;
    const pressed=(frame.down&&!__weLastPointer.down&&!cancelled?frame.hits:__weLastPointer.pressed)
        .filter(node=>!frame.eligible||frame.eligible.includes(node));
    for(let i=0;i<__weScripts.length;i++) {
        const node=__weScripts[i].node,was=__weLastPointer.hits.includes(node),hit=frame.hits.includes(node);
        if(!was&&!hit)continue;
        const event={worldPosition:input.cursorWorldPosition,localPosition:__weVector(frame.locals?.[node]||[0,0,0],3),screenPosition:input.cursorScreenPosition};
        if(was&&!hit) __weCall(i,'cursorLeave',event);
        if(hit&&!was) __weCall(i,'cursorEnter',event);
        if(hit&&(frame.pointer[0]!==__weLastPointer.position[0]||frame.pointer[1]!==__weLastPointer.position[1])) __weCall(i,'cursorMove',event);
        if(hit&&frame.down&&!__weLastPointer.down&&!cancelled) __weCall(i,'cursorDown',event);
        if(hit&&frame.focused&&!frame.down&&__weLastPointer.down&&!cancelled) {
            __weCall(i,'cursorUp',event);if(pressed.includes(node)) __weCall(i,'cursorClick',event);
        }
    }
    __weLastPointer={focused:frame.focused,down:frame.down,hits:frame.hits,pressed:frame.focused&&frame.down?pressed:[],position:frame.pointer,cancelled:cancelled&&(!frame.focused||frame.down)};
}
function __weFrame(frame) {
    engine.runtime=frame.time;engine.frametime=frame.delta;
    if(frame.cameraPose&&__weSceneIndex>=0) {
        __weSyncing=true;
        try{__weNodes[__weSceneIndex].__cameraPose=frame.cameraPose;}
        finally{__weSyncing=false;}
    }
    if(frame.mediaTimeline && (!__weLastTimeline || Math.abs(frame.mediaTimeline.position-__weLastTimeline.position)>=0.25 || frame.mediaTimeline.duration!==__weLastTimeline.duration)) {
        __weLastTimeline=frame.mediaTimeline;__weEvent('mediaTimelineChanged',frame.mediaTimeline);
    }
    __weSyncValues(frame.values??[]);
    for(const pose of frame.poses??[])__weModelPoses.set(pose.node,pose);
    for(const event of frame.animationEvents??[]) {
        if(event.event.key!==undefined&&!__weModelAnimationEvent(event.node,event.event))continue;
        for(let i=0;i<__weScripts.length;i++)if(__weScripts[i].node===event.node)__weCall(i,'animationEvent',event.event);
    }
    for(const event of frame.videoEvents??[])__weVideoEnded(event);
    if(frame.timeOfDay==null) {const date=new Date();engine.timeOfDay=(date.getHours()*3600+date.getMinutes()*60+date.getSeconds()+date.getMilliseconds()/1000)/86400;} else engine.timeOfDay=frame.timeOfDay;
    if(engine.screenResolution.x!==frame.screen[0]||engine.screenResolution.y!==frame.screen[1]) {
        const initialized=engine.screenResolution.x!==0;
        engine.screenResolution.x=frame.screen[0];engine.screenResolution.y=frame.screen[1];
        if(initialized) __weEvent('resizeScreen',engine.screenResolution);
    }
    if(frame.audio) for(const [count,view] of __weAudioViews) {
        const spectrum=frame.audio.bands[[16,32,64].indexOf(count)];
        for(const channel of ['left','right','average']) view[channel].set(spectrum[channel]);
    }
    for(const event of frame.pointerEvents??[]) __wePointer(event);
    __wePointer(frame);
    if(frame.delta>0) for(const timer of [...__weTimers]) if(timer.due<=engine.runtime) {
        if(timer.repeat) timer.due=engine.runtime+timer.delay;else __weTimers.delete(timer);
        if(timer.owner>=0){if(!__weActive(timer.owner)){__weTimers.delete(timer);continue;}__weActivate(timer.owner);}
        try{timer.callback();}catch(error){__weTimers.delete(timer);__weLog('SceneScript timer: '+String(error.stack||error));}
    }
    for(let i=0;i<__weScripts.length;i++) __weCall(i,'update');
    return __weFlush();
}
function __weProperties(values,changed,overrides) {
    engine.userProperties=__weUserProperties(values,engine.userProperties);
    for(const entry of overrides) {
        const script=__weScripts.find(s=>s.node===entry.node&&JSON.stringify(s.path)===JSON.stringify(entry.path));
        const properties=script?.namespace.scriptProperties;
        if(properties&&!script.disabled&&!__weNodes[entry.node].__destroyed) for(const [key,value] of Object.entries(entry.properties)) properties[key]=properties[key] instanceof WEVec?__weVector(value,properties[key]._n):value;
    }
    __weEvent('applyUserProperties',__weUserProperties(changed));
    return __weFlush();
}
function __weShutdown() { __weEvent('destroy');__weTimers.clear(); }
globalThis.__weBuiltins={WEMath,WEVector,WEColor};
