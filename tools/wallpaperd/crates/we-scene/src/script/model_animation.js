'use strict';
// Animation slots keep property-script paths stable when other layers are removed.
let __weAnimationKey=0;
const __weAnimationHandles=new WeakMap(),__weEndedCallbacks=new Map();
function __weAnimationDisplay(frame,clip,single) {
    const span=clip.frames;if(span<=0)return 0;
    if(!single&&clip.mode==='loop')return ((frame%span)+span)%span;
    if(!single&&(clip.mode==='mirror'||clip.mode==='pingpong')){const f=((frame%(2*span))+2*span)%(2*span);return f>span?2*span-f:f;}
    return Math.max(0,Math.min(span,frame));
}
function __weInstallModelAnimations(raw,index) {
    const clips=raw.__model.clips;
    raw.animationlayers??=clips.length?[{animation:clips[0].id,name:clips[0].name}]:[];
    if(raw.animationlayers.length>128)throw new RangeError('Too many skeletal animation layers');
    for(let slot=0;slot<raw.animationlayers.length;slot++){
        const layer=raw.animationlayers[slot];
        layer.__key='skeletal:'+(__weAnimationKey++);layer.__order=slot;
        layer.playing??=true;layer.paused??=layer.startpaused??false;layer.__revision??=0;
        if(raw.__createdAt!==undefined){layer.__frame??=0;layer.__time??=raw.__createdAt;}
    }
    __weMark(index,['animationlayers'],raw.animationlayers);
    function active(){__weCheck(index);return raw.animationlayers.filter(l=>!l.__destroyed).sort((a,b)=>a.__order-b.__order);}
    function find(key){return typeof key==='number'?active()[key]:active().find(l=>l.name===key);}
    function clipFor(layer){return clips.find(c=>c.id===layer.animation||c.name===layer.animation);}
    function checked(key){__weCheck(index);const layer=raw.animationlayers.find(l=>l.__key===key&&!l.__destroyed);if(!layer)throw new ReferenceError('Animation layer handle has been destroyed');return layer;}
    function rawFrame(layer,clip){
        const pose=__weModelPoses.get(index),state=pose?.layers?.find(l=>l.key===layer.__key&&l.revision===layer.__revision);
        const moving=layer.playing&&!layer.paused;
        let frame,anchor;
        if(state){frame=state.frame;anchor=pose.time;}
        else if(layer.__frame!==undefined){frame=layer.__frame;anchor=layer.__time;}
        else {frame=moving?engine.runtime*clip.fps*(layer.rate??1):0;anchor=engine.runtime;}
        if(moving&&anchor!==undefined)frame+=Math.max(0,engine.runtime-anchor)*clip.fps*(layer.rate??1);
        return frame;
    }
    function frameOf(layer,clip){return __weAnimationDisplay(rawFrame(layer,clip),clip,layer.__single);}
    function handle(key){
        const first=checked(key),clip=clipFor(first);if(!clip)return undefined;
        function state(){return checked(key);}
        function seek(frame){const l=state();if(!Number.isFinite(frame)||Math.abs(frame)>1e12)throw new RangeError('Invalid animation frame');l.__frame=frame;l.__time=engine.runtime;l.__revision++;}
        function anchor(){const l=state();seek(rawFrame(l,clip));return l;}
        const h=Object.freeze({
            get fps(){state();return clip.fps;},get frameCount(){state();return clip.frames;},get duration(){state();return clip.duration;},
            get name(){return state().name??clip.name;},
            get rate(){return state().rate??1;},set rate(v){if(!Number.isFinite(v)||Math.abs(v)>128)throw new RangeError('Invalid animation rate');anchor().rate=v;},
            get blend(){return state().blend??1;},set blend(v){if(!Number.isFinite(v)||v<0||v>1)throw new RangeError('Invalid animation blend');state().blend=v;},
            get visible(){return state().visible!==false;},set visible(v){state().visible=!!v;},
            play(){const l=anchor();if(!l.playing||(l.__single||clip.mode==='single')&&l.__frame>=clip.frames)seek(0);l.playing=true;l.paused=false;},
            pause(){anchor().paused=true;},
            stop(){seek(0);const l=state();l.playing=false;l.paused=true;},
            isPlaying(){const l=state();return l.playing&&!l.paused&&(l.rate??1)!==0&&(!(l.__single||clip.mode==='single')||((l.rate??1)>=0?frameOf(l,clip)<clip.frames:frameOf(l,clip)>0));},
            getFrame(){const l=state();return frameOf(l,clip);},setFrame:seek,
            addEndedCallback(callback){state();if(typeof callback!=='function')throw new TypeError('Invalid ended callback');const id=index+':'+key;let callbacks=__weEndedCallbacks.get(id);if(!callbacks)__weEndedCallbacks.set(id,callbacks=[]);if(callbacks.length>=128)throw new RangeError('Too many animation callbacks');callbacks.push({callback,owner:__weCurrent});},
            // Property timelines nested in animation-layer config remain independent.
            getAnimation(name){const l=state(),slot=raw.animationlayers.indexOf(l);return __weAnimationFor(index,['animationlayers',String(slot)],name);},
        });
        __weAnimationHandles.set(h,{index,key});return h;
    }
    function destroy(key){
        __weCheck(index);let layer;
        if(key&&typeof key==='object'){const reference=__weAnimationHandles.get(key);if(reference?.index===index)layer=raw.animationlayers.find(l=>l.__key===reference.key&&!l.__destroyed);}
        else layer=find(key);
        if(!layer)return false;
        layer.__destroyed=true;__weEndedCallbacks.delete(index+':'+layer.__key);return true;
    }
    function create(animation,config={},single=false){
        __weCheck(index);
        const value=typeof animation==='string'?{animation}:animation;
        if(!value||typeof value!=='object'||!config||typeof config!=='object')throw new TypeError('Invalid animation config');
        const spec={...value,...config},clip=clipFor(spec);
        if(!clip)throw new RangeError('Unknown skeletal animation: '+spec.animation);
        const layers=active();if(layers.length>=128)throw new RangeError('Too many skeletal animation layers');
        const rate=spec.rate??1,blend=spec.blend??1,blendtime=spec.blendtime??0;
        if(!Number.isFinite(rate)||Math.abs(rate)>128||!Number.isFinite(blend)||blend<0||blend>1||!Number.isFinite(blendtime)||blendtime<0||blendtime>3600)throw new RangeError('Invalid animation config values');
        const slot=raw.animationlayers.findIndex(l=>l.__destroyed);
        const key='skeletal:'+(__weAnimationKey++);
        const layer={...spec,animation:clip.id,name:spec.name??clip.name,rate,blend,blendtime,__key:key,__order:layers.length,__single:single,__frame:0,__time:engine.runtime,__revision:1,playing:true,paused:spec.startpaused??false};
        if(slot<0)raw.animationlayers.push(layer);else raw.animationlayers[slot]=layer;
        const added=raw.animationlayers.find(l=>l.__key===key);
        if(spec.autosort){const before=layers.findIndex(l=>l.additive);layers.splice(before<0?layers.length:before,0,added);for(let i=0;i<layers.length;i++)layers[i].__order=i;}
        return handle(key);
    }
    Object.defineProperties(raw,{
        getAnimationLayerCount:{value:()=>active().length},
        getAnimationLayer:{value:key=>{const layer=find(key);return layer?handle(layer.__key):undefined;}},
        createAnimationLayer:{value:(animation,config)=>create(animation,config)},
        playSingleAnimation:{value:(animation,config)=>create(animation,config,true)},
        destroyAnimationLayer:{value:destroy},
    });
}
function __weModelAnimationEvent(index,event) {
    const node=__weNodes[index];if(!node||node.__destroyed)return false;
    if(!event.ended)return true;
    const layer=node.animationlayers?.find(l=>l.__key===event.key&&!l.__destroyed&&l.__revision===event.revision);
    if(!layer)return false;
    for(const {callback,owner} of [...(__weEndedCallbacks.get(index+':'+event.key)??[])]) {
        if(owner>=0){if(!__weActive(owner))continue;__weActivate(owner);}
        try{callback();}catch(error){if(owner>=0)__weScripts[owner].disabled=true;__weLog('SceneScript animation callback: '+String(error)+'\n'+String(error.stack||''));}
    }
    if(layer.__single&&!layer.__destroyed&&layer.__revision===event.revision){layer.__destroyed=true;__weEndedCallbacks.delete(index+':'+event.key);}
    return false;
}
