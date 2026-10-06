const __weVideoStates=new Map(),__weVideoCallbacks=new Map();
function __weVideoTime(s) {
    const raw=s.joined?engine.runtime*s.rate:s.position+(s.playing?(engine.runtime-s.anchor)*s.rate:0);
    return s.duration>0?(s.loop?((raw%s.duration)+s.duration)%s.duration:Math.max(0,Math.min(raw,s.duration))):Math.max(0,raw);
}
function __weInstallVideo(raw,index) {
    const master=raw.__videoMaster;
    const state=()=>{__weCheck(index);return __weVideoStates.get(master);};
    const hold=s=>{s.position=__weVideoTime(s);s.anchor=engine.runtime;s.joined=false;};
    const mark=s=>{s.revision++;__weMark(master,['__texture'],s);};
    const handle=Object.freeze({
        get duration(){return state().duration;},
        get rate(){return state().rate;},
        set rate(value){if(!Number.isFinite(value)||Math.abs(value)>100)throw new RangeError('Invalid video rate');const s=state();hold(s);s.rate=value;mark(s);},
        get loop(){return state().loop;},
        set loop(value){if(typeof value!=='boolean')throw new TypeError('Invalid video loop flag');const s=state();hold(s);s.loop=value;mark(s);},
        play(){const s=state();hold(s);if(!s.loop&&s.duration>0){if(s.rate<0&&s.position<=0)s.position=s.duration;else if(s.rate>0&&s.position>=s.duration)s.position=0;}s.playing=true;mark(s);},
        pause(){const s=state();hold(s);s.playing=false;mark(s);},
        stop(){const s=state();s.position=0;s.anchor=engine.runtime;s.joined=false;s.playing=false;mark(s);},
        isPlaying(){const s=state();return s.playing&&s.rate!==0&&(s.loop||(s.rate<0?__weVideoTime(s)>0:__weVideoTime(s)<s.duration));},
        getCurrentTime(){return __weVideoTime(state());},
        setCurrentTime(value){const s=state();if(!Number.isFinite(value)||value<0||value>1e9)throw new RangeError('Invalid video time');s.position=s.duration>0?Math.min(value,s.duration):value;s.anchor=engine.runtime;s.joined=false;mark(s);},
        addEndedCallback(callback){state();const callbacks=__weVideoCallbacks.get(master)||[];if(typeof callback!=='function'||callbacks.length>=128)throw new RangeError('Invalid or excessive video callbacks');callbacks.push({callback,owner:__weCurrent,index});__weVideoCallbacks.set(master,callbacks);},
    });
    Object.defineProperty(raw,'getVideoTexture',{value:()=>{state();return handle;}});
}
function __weVideoEnded(event) {
    for(const item of __weVideoCallbacks.get(event.node)||[]) {
        if(__weNodes[item.index].__destroyed||item.owner>=0&&!__weActive(item.owner))continue;
        if(item.owner>=0)__weActivate(item.owner);
        try{item.callback();}catch(error){if(item.owner>=0)__weScripts[item.owner].disabled=true;__weLog('SceneScript video ended: '+String(error.stack||error));}
    }
}
