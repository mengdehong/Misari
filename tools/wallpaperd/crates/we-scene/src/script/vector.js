'use strict';
const __weKeys = ['x', 'y', 'z', 'w'];
class WEVec {
    constructor(n, args) {
        Object.defineProperty(this, '_n', {value:n});
        let values;
        const v = args[0];
        if (typeof v === 'string') values = v.trim().split(/\s+/).map(Number);
        else if (v instanceof WEVec || Array.isArray(v)) {
            values = v instanceof WEVec ? v.toArray() : v.slice();
            for (let i=1; i<args.length; i++) values.push(args[i]);
        } else if (v && typeof v === 'object') values = __weKeys.slice(0,n).map(k=>v[k] ?? 0);
        else if (args.length === 1) values = Array(n).fill(v);
        else values = args;
        for (let i=0;i<n;i++) this[__weKeys[i]] = Number(values[i] ?? 0);
    }
    toArray() { return __weKeys.slice(0,this._n).map(k=>this[k]); }
    _map(fn) { return new this.constructor(...this.toArray().map(fn)); }
    _zip(v, fn) { return this._map((x,i)=>fn(x,typeof v==='number'?v:(v[__weKeys[i]] ?? 0))); }
    copy() { return this._map(x=>x); }
    isFinite() { return this.toArray().every(Number.isFinite); }
    equals(v) { return this.toArray().every((x,i)=>Math.abs(x-v[__weKeys[i]])<0.00001); }
    negate() { return this._map(x=>-x); }
    add(v) { return this._zip(v,(x,y)=>x+y); }
    subtract(v) { return this._zip(v,(x,y)=>x-y); }
    multiply(v) { return this._zip(v,(x,y)=>x*y); }
    divide(v) { return this._zip(v,(x,y)=>x/y); }
    lengthSqr() { return this.dot(this); }
    length() { return Math.sqrt(this.lengthSqr()); }
    normalize() { const l=this.length(); return l===0?this._map(()=>0):this.divide(l); }
    distanceSqr(v) { return this.subtract(v).lengthSqr(); }
    distance(v) { return Math.sqrt(this.distanceSqr(v)); }
    dot(v) { return this.toArray().reduce((s,x,i)=>s+x*v[__weKeys[i]],0); }
    reflect(n) { return this.subtract(n.multiply(2*this.dot(n))); }
    refract(n, eta) { const d=this.dot(n), k=1-eta*eta*(1-d*d); return k<0?this._map(()=>0):this.multiply(eta).subtract(n.multiply(eta*d+Math.sqrt(k))); }
    project(v) { const d=v.lengthSqr(); return d===0?this._map(()=>0):v.multiply(this.dot(v)/d); }
    angleBetween(v) { const d=this.length()*v.length(); return d===0?0:Math.acos(Math.min(1,Math.max(-1,this.dot(v)/d)))*180/Math.PI; }
    mix(v,a) { return this.add(v.subtract(this).multiply(a)); }
    min(v) { return this._zip(v,Math.min); }
    max(v) { return this._zip(v,Math.max); }
    clamp(lo,hi) { return this.max(lo).min(hi); }
    abs() { return this._map(Math.abs); }
    sign() { return this._map(Math.sign); }
    round() { return this._map(Math.round); }
    floor() { return this._map(Math.floor); }
    ceil() { return this._map(Math.ceil); }
    fract() { return this._map(x=>x-Math.floor(x)); }
    mod(v) { return this._zip(v,(x,y)=>x-y*Math.floor(x/y)); }
    step(edge) { return this._zip(edge,(x,y)=>x<y?0:1); }
    smoothStep(lo,hi) { return this.subtract(lo).divide(__weBinary('-',hi,lo)).clamp(0,1)._map(x=>x*x*(3-2*x)); }
    toString() { return this.toArray().join(' '); }
    toJSON() { return this.toArray(); }
}
class Vec2 extends WEVec {
    constructor(...v) { super(2,v); }
    perpendicular() { return new Vec2(-this.y,this.x); }
}
class Vec3 extends WEVec {
    constructor(...v) { super(3,v); }
    cross(v) { return new Vec3(this.y*v.z-this.z*v.y,this.z*v.x-this.x*v.z,this.x*v.y-this.y*v.x); }
    static fromSpherical(r,theta,phi) { theta*=Math.PI/180;phi*=Math.PI/180;return new Vec3(r*Math.sin(theta)*Math.cos(phi),r*Math.cos(theta),r*Math.sin(theta)*Math.sin(phi)); }
    toSpherical() { const r=this.length();return new Vec3(r,r?Math.acos(this.y/r)*180/Math.PI:0,Math.atan2(this.z,this.x)*180/Math.PI); }
}
class Vec4 extends WEVec { constructor(...v) { super(4,v); } }
function __weBinary(op,a,b) {
    if(a instanceof WEMatrix || b instanceof WEMatrix) return __weMatrixBinary(op,a,b);
    if (a instanceof WEVec || b instanceof WEVec) {
        const vector = a instanceof WEVec ? a : b;
        const left = a instanceof WEVec ? a : new vector.constructor(a);
        const method = {'+':'add','-':'subtract','*':'multiply','/':'divide'}[op];
        return left[method](b);
    }
    switch(op) { case '+':return a+b;case '-':return a-b;case '*':return a*b;case '/':return a/b; }
    throw new TypeError('Invalid arithmetic operator');
}
function __weUnary(op,v) { if(v instanceof WEMatrix)return op==='-'?v.multiply(-1):v.copy();return v instanceof WEVec?(op==='-'?v.negate():v.copy()):(op==='-'?-v:+v); }
function __weRef(object,key) {
    // ToPropertyKey and the getter must run once, before the right-hand side.
    key=typeof key==='symbol'?key:String(key);
    const value=object[key];
    return {assign(op,right) { return object[key]=__weBinary(op,value,right); }};
}
const WEMath = {
    deg2rad:Math.PI/180, rad2deg:180/Math.PI,
    mix:(a,b,x)=>a+(b-a)*x,
    smoothStep:(lo,hi,x)=>{x=Math.max(0,Math.min(1,(x-lo)/(hi-lo)));return x*x*(3-2*x);},
};
const WEVector = {
    angleVector2:a=>new Vec2(Math.cos(a*Math.PI/180),Math.sin(a*Math.PI/180)),
    vectorAngle2:v=>Math.atan2(v.y,v.x)*180/Math.PI,
};
const WEColor = {
    normalizeColor:c=>c.divide(255), expandColor:c=>c.multiply(255),
    rgb2hsv:c=>{
        const max=Math.max(c.x,c.y,c.z),min=Math.min(c.x,c.y,c.z),d=max-min;
        let h=0;
        if(d) h=max===c.x?((c.y-c.z)/d)%6:max===c.y?(c.z-c.x)/d+2:(c.x-c.y)/d+4;
        return new Vec3((h/6+1)%1,max?d/max:0,max);
    },
    hsv2rgb:c=>{
        const h=((c.x%1)+1)%1*6,s=c.y,v=c.z,i=Math.floor(h),f=h-i;
        const p=v*(1-s),q=v*(1-f*s),t=v*(1-(1-f)*s);
        return new Vec3(...[[v,t,p],[q,v,p],[p,v,t],[p,q,v],[t,p,v],[v,p,q]][i%6]);
    },
};
