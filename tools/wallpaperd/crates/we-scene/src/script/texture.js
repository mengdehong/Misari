function __weInstallTexture(raw,index) {
    if(!raw.__texture)return;
    if(raw.__videoMaster===index)__weVideoStates.set(index,raw.__texture);
    if(raw.__videoMaster!==undefined)__weInstallVideo(raw,index);
    else Object.defineProperty(raw,'getVideoTexture',{value:()=>{__weCheck(index);return undefined;}});
    const state=()=>{__weCheck(index);return __weNodes[index].__texture;};
    const time=s=>s.joined?engine.runtime*s.rate:s.position+(s.playing?(engine.runtime-s.anchor)*s.rate:0);
    const hold=s=>{s.position=time(s);s.anchor=engine.runtime;s.joined=false;};
    const handle=Object.freeze({
        get frameCount(){return state().frameCount;},get duration(){return state().duration;},
        get rate(){return state().rate;},set rate(value){if(!Number.isFinite(value)||Math.abs(value)>128)throw new RangeError('Invalid texture animation rate');const s=state();hold(s);s.rate=value;},
        play(){const s=state();hold(s);s.playing=true;},
        pause(){const s=state();hold(s);s.playing=false;},
        stop(){const s=state();s.position=0;s.anchor=engine.runtime;s.joined=false;s.playing=false;},
        isPlaying(){return state().playing;},
        getFrame(){const s=state();let position=((time(s)%s.duration)+s.duration)%s.duration;for(let i=0;i<s.durations.length;i++){if(position<s.durations[i])return i+position/s.durations[i];position-=s.durations[i];}return 0;},
        setFrame(frame){const s=state();if(!Number.isFinite(frame)||frame<0||frame>=s.frameCount)throw new RangeError('Invalid texture animation frame');const i=Math.floor(frame);s.position=s.durations.slice(0,i).reduce((a,b)=>a+b,0)+s.durations[i]*(frame-i);s.anchor=engine.runtime;s.joined=false;},
        join(){const s=state();s.joined=true;s.playing=true;},
    });
    Object.defineProperty(raw,'getTextureAnimation',{value:()=>{
        const s=state();if(!s.loaded)Object.assign(s,JSON.parse(__weTextureInfo(s.asset)));
        return s.frameCount?handle:undefined;
    }});
}
