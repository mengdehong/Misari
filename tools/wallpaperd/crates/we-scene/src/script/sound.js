function __weInstallSound(raw,index){
    if(!raw.__sound)return;
    const state=()=>{__weCheck(index);return __weNodes[index].__sound;};
    Object.defineProperties(raw,{
        isPlaying:{value:()=>state().playing&&!state().paused},
        play:{value:()=>{const s=state();s.restart=!s.playing;s.playing=true;s.paused=false;s.__time=engine.runtime;s.revision++;}},
        pause:{value:()=>{const s=state();s.paused=true;s.restart=false;s.__time=engine.runtime;s.revision++;}},
        stop:{value:()=>{const s=state();s.playing=false;s.paused=false;s.restart=true;s.__time=engine.runtime;s.revision++;}},
    });
}
