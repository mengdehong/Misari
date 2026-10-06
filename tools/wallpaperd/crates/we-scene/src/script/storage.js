class ILocalStorage { static LOCATION_GLOBAL='global';static LOCATION_SCREEN='screen'; }
const localStorage=Object.freeze({
    LOCATION_GLOBAL:'global', LOCATION_SCREEN:'screen',
    get(key,location='screen') {
        const value=__weStorage('get',String(key),'',location);
        return value===undefined?undefined:JSON.parse(value,(key,v)=>v?.__wallpaperdVector?__weVector(v.components,v.__wallpaperdVector):v);
    },
    set(key,value,location='screen') {
        const data=JSON.stringify({value},function(key,v){const original=this[key];return original instanceof WEVec?{__wallpaperdVector:original._n,components:original.toJSON()}:v;});
        if(data===undefined||!Object.hasOwn(JSON.parse(data),'value'))throw new TypeError('Storage requires a JSON value');
        __weStorage('set',String(key),JSON.stringify(JSON.parse(data).value),location);
    },
    delete(key,location='screen'){return JSON.parse(__weStorage('delete',String(key),'',location));},
    clear(location='screen'){__weStorage('clear','','',location);},
});
